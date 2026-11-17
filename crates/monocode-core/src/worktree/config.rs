//! `monocode.yaml` (else `orca.yaml`) at a checkout's root; same schema as Orca:
//!
//! ```yaml
//! worktree:
//!   sharedDirectories: [node_modules, .cache]
//! scripts:
//!   setup: |
//!     npm install
//!   archive: ./cleanup.sh
//! ```
//!
//! Unknown keys are ignored. A file larger than [`MAX_BYTES`], malformed YAML, or YAML
//! using anchors/aliases yields an empty config with a warning. When `monocode.yaml`
//! exists it is the only file read, even if it is malformed.
use std::path::{Path, PathBuf};
use yaml_rust2::{parser::Event, parser::Parser, Yaml, YamlLoader};

pub const FILES: [&str; 2] = ["monocode.yaml", "orca.yaml"];
pub const MAX_BYTES: u64 = 256 * 1024;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// Trimmed `scripts.setup`.
    pub setup: Option<String>,
    /// Trimmed `scripts.archive` (parsed for parity; not run yet).
    pub archive: Option<String>,
    /// Normalized, safe relative `worktree.sharedDirectories`.
    pub shared_directories: Vec<String>,
    /// The file the config came from.
    pub source: Option<PathBuf>,
    pub warnings: Vec<String>,
}

/// Read `dir/monocode.yaml`, else `dir/orca.yaml`. Never fails.
pub fn load(dir: &Path) -> Config {
    for name in FILES {
        let path = dir.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let mut config = if meta.len() > MAX_BYTES {
            Config {
                warnings: vec![format!("{name} is larger than 256 KiB and was ignored.")],
                ..Config::default()
            }
        } else {
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| parse(&t))
            {
                Ok(config) => config,
                Err(e) => Config {
                    warnings: vec![format!("{name} could not be read and was ignored: {e}")],
                    ..Config::default()
                },
            }
        };
        config.source = Some(path);
        return config;
    }
    Config::default()
}

/// Refuse aliases (exponential expansion) and absurd nesting before building the tree.
fn prescan(text: &str) -> Result<(), String> {
    let mut parser = Parser::new_from_str(text);
    let mut depth = 0usize;
    loop {
        let (event, _) = parser.next_token().map_err(|e| e.to_string())?;
        match event {
            Event::StreamEnd => return Ok(()),
            Event::Alias(_) => return Err("YAML anchors and aliases are not supported".into()),
            Event::SequenceStart(..) | Event::MappingStart(..) => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err("YAML is nested too deeply".into());
                }
            }
            Event::SequenceEnd | Event::MappingEnd => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
}

fn trimmed(value: &Yaml) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A relative path inside the checkout: no absolute, `..`, or `.git` segments.
pub(crate) fn safe_relative(raw: &str) -> Option<String> {
    let normalized = raw.trim().replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    let normalized = normalized.strip_prefix("./").unwrap_or(normalized);
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.as_bytes().get(1) == Some(&b':')
    {
        return None;
    }
    let segments: Vec<&str> = normalized.split('/').filter(|s| *s != ".").collect();
    if segments.is_empty()
        || segments.iter().any(|s| s.is_empty() || *s == "..")
        || segments[0] == ".git"
    {
        return None;
    }
    Some(segments.join("/"))
}

pub fn parse(text: &str) -> Result<Config, String> {
    if text.len() as u64 > MAX_BYTES {
        return Err("larger than 256 KiB".into());
    }
    prescan(text)?;
    let docs = YamlLoader::load_from_str(text).map_err(|e| e.to_string())?;
    let mut config = Config::default();
    let Some(root) = docs.into_iter().next() else {
        return Ok(config);
    };
    if root.is_null() {
        return Ok(config);
    }
    if root.as_hash().is_none() {
        return Err("the top level must be a mapping".into());
    }
    config.setup = trimmed(&root["scripts"]["setup"]);
    config.archive = trimmed(&root["scripts"]["archive"]);
    let shared = &root["worktree"]["sharedDirectories"];
    let entries: Vec<&Yaml> = match shared {
        Yaml::Array(items) => items.iter().collect(),
        Yaml::String(_) => vec![shared],
        _ => Vec::new(),
    };
    for entry in entries {
        let Some(raw) = entry.as_str() else {
            config
                .warnings
                .push("Skipping a sharedDirectories entry that is not a string.".into());
            continue;
        };
        match safe_relative(raw) {
            Some(p) if !config.shared_directories.contains(&p) => config.shared_directories.push(p),
            Some(_) => {}
            None => config.warnings.push(format!(
                "Skipping unsafe sharedDirectories entry \"{raw}\"."
            )),
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_config_parse_and_precedence() {
        let c = parse(
            "worktree:\n  sharedDirectories: [node_modules, ./.cache/, ../x, /abs, .git/hooks, a//b, node_modules, 3]\n  future: 1\nscripts:\n  setup: |\n    npm install\n    npm run build\n  archive: '  '\nunknown: {a: b}\n",
        )
        .unwrap();
        assert_eq!(c.setup.as_deref(), Some("npm install\nnpm run build"));
        assert_eq!(c.archive, None);
        assert_eq!(c.shared_directories, ["node_modules", ".cache"]);
        assert_eq!(c.warnings.len(), 5, "{:?}", c.warnings);
        assert_eq!(parse("").unwrap(), Config::default());
        assert_eq!(parse("# only a comment\n").unwrap(), Config::default());
        assert!(parse("- a\n- b\n").is_err());
        assert!(parse("a: &x [1]\nb: *x\n").unwrap_err().contains("alias"));
        assert!(parse(&format!("{}1{}", "[".repeat(100), "]".repeat(100))).is_err());

        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Config::default());
        std::fs::write(dir.path().join("orca.yaml"), "scripts:\n  setup: orca\n").unwrap();
        let c = load(dir.path());
        assert_eq!(c.setup.as_deref(), Some("orca"));
        assert!(c.source.unwrap().ends_with("orca.yaml"));
        std::fs::write(
            dir.path().join("monocode.yaml"),
            "scripts:\n  setup: mono\n",
        )
        .unwrap();
        assert_eq!(load(dir.path()).setup.as_deref(), Some("mono"));
        // Malformed monocode.yaml → warning and empty config (orca.yaml is not consulted).
        std::fs::write(dir.path().join("monocode.yaml"), "scripts: [unclosed\n").unwrap();
        let c = load(dir.path());
        assert_eq!(c.setup, None);
        assert_eq!(c.warnings.len(), 1);
        assert!(c.warnings[0].contains("monocode.yaml"));
        // Oversized file.
        let big = format!(
            "scripts:\n  setup: x\n#{}\n",
            "a".repeat(MAX_BYTES as usize)
        );
        std::fs::write(dir.path().join("monocode.yaml"), big).unwrap();
        let c = load(dir.path());
        assert_eq!(c.setup, None);
        assert!(c.warnings[0].contains("256 KiB"));
    }
}
