//! Connections other tools on this machine already know, offered as data
//! sources so nobody types them in twice:
//!
//! - dbports (`~/.config/dbports/connections.yaml` tunnels and
//!   `databases.yaml` connections, with users and passwords);
//! - SOCket (`~/.config/socradar-socket/connections.json`: proxy commands
//!   and kubectl port-forwards);
//! - the shell's start-up files (aliases and functions that run
//!   `cloud-sql-proxy`, `alloydb-auth-proxy` or `kubectl port-forward`);
//! - proxies and port-forwards running right now.
//!
//! Nothing here connects anywhere or changes a file; it only reads.

use super::source::{DataSource, DbColor, DbKind, McpAccess, Tunnel, TunnelKind};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One connection found, ready to be saved as a data source.
#[derive(Debug, Clone)]
pub struct Found {
    pub source: DataSource,
    /// A password the other tool keeps (dbports does, in plain text).
    pub password: Option<String>,
    /// Where it was found: "dbports", "SOCket", "~/.zshrc", "running".
    pub origin: String,
}

/// Every connection found in `home` and in the running processes that is
/// not a data source yet. The better-described copy wins when two places
/// know the same port (dbports has users and databases, a running process
/// has only a port).
pub fn discover(home: &Path, existing: &[DataSource]) -> Vec<Found> {
    let mut found = Vec::new();
    found.extend(dbports(home));
    found.extend(socket(home));
    for file in SHELL_FILES {
        let path = home.join(file);
        if let Ok(text) = std::fs::read_to_string(&path) {
            found.extend(shell_file(&text, &format!("~/{file}")));
            for sourced in sourced_files(&text, home) {
                if let Ok(text) = std::fs::read_to_string(&sourced) {
                    let shown = sourced
                        .strip_prefix(home)
                        .map(|p| format!("~/{}", p.display()))
                        .unwrap_or_else(|_| sourced.display().to_string());
                    found.extend(shell_file(&text, &shown));
                }
            }
        }
    }
    found.extend(running());
    dedupe(found, existing)
}

const SHELL_FILES: [&str; 6] = [
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".bashrc",
    ".bash_profile",
    ".profile",
];

/// Files a start-up file reads with `source` or `.`, one level deep, as
/// SOCket follows them: only files under the home folder, with no
/// variables left, and not a plugin manager's.
fn sourced_files(text: &str, home: &Path) -> Vec<PathBuf> {
    const SKIP: [&str; 7] = [
        "oh-my-zsh",
        "google-cloud-sdk",
        "nvm",
        ".inc",
        "zsh-syntax",
        "zsh-autosuggestions",
        "iterm2",
    ];
    let home_text = home.to_string_lossy();
    let mut files = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((verb, path)) = line.split_once(' ') else {
            continue;
        };
        if verb != "source" && verb != "." {
            continue;
        }
        let path = path.trim().trim_matches(['\'', '"', ' ']);
        let path = path
            .replace("${HOME}", &home_text)
            .replace("$HOME", &home_text);
        let path = match path.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(path),
        };
        let shown = path.to_string_lossy();
        if !path.starts_with(home)
            || shown.contains('$')
            || SKIP.iter().any(|skip| shown.contains(skip))
            || !path.is_file()
        {
            continue;
        }
        files.push(path);
    }
    files
}

/// Keeps the first copy of each connection, and none that is saved already.
/// A connection is its port, database and user; one with neither database
/// nor user is a bare tunnel, the same as any other on its port that runs
/// the same instance (two tunnels may take turns on one port).
fn dedupe(found: Vec<Found>, existing: &[DataSource]) -> Vec<Found> {
    let key = |s: &DataSource| (s.port, s.database.to_lowercase(), s.user.to_lowercase());
    let target = |s: &DataSource| {
        s.tunnel
            .as_ref()
            .map(|t| format!("{}{}", t.instance, t.command))
            .unwrap_or_default()
    };
    let bare = |s: &DataSource| s.database.is_empty() && s.user.is_empty();
    let mut kept_sources: Vec<DataSource> = existing.to_vec();
    let mut seen: HashSet<(u16, String, String)> = existing.iter().map(key).collect();
    let mut kept = Vec::new();
    for f in found {
        let same_place = |other: &DataSource| {
            other.port == f.source.port
                && (other.tunnel.is_none()
                    || f.source.tunnel.is_none()
                    || target(other) == target(&f.source))
        };
        if bare(&f.source) && kept_sources.iter().any(same_place) {
            continue;
        }
        if !bare(&f.source) && !seen.insert(key(&f.source)) {
            continue;
        }
        kept_sources.push(f.source.clone());
        kept.push(f);
    }
    kept
}

/// A data source on the loopback, through `tunnel`, on `port`.
fn tunneled(name: &str, port: u16, tunnel: Tunnel) -> DataSource {
    let mongo = looks_like_mongo(name) || looks_like_mongo(&tunnel.instance) || is_mongo_port(port)
        || tunnel.remote_port.is_some_and(is_mongo_port);
    let mut source = DataSource::new_of(
        if mongo {
            DbKind::MongoDb
        } else {
            DbKind::Postgres
        },
        name,
    );
    source.host = "127.0.0.1".into();
    source.port = port;
    source.tunnel = Some(tunnel);
    source.color = color_for(name);
    source
}

fn looks_like_mongo(text: &str) -> bool {
    text.to_lowercase().contains("mongo")
}

fn is_mongo_port(port: u16) -> bool {
    (27017..=27019).contains(&port)
}

/// Production in red, pre-production in orange: the colour says where a
/// statement is about to go.
fn color_for(text: &str) -> Option<DbColor> {
    let text = text.to_lowercase();
    let has = |word: &str| {
        text.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|part| part == word)
    };
    if has("preprod") || has("staging") || has("stage") {
        Some(DbColor::Orange)
    } else if has("prod") || has("production") || has("live") {
        Some(DbColor::Red)
    } else if has("test") || has("qa") {
        Some(DbColor::Yellow)
    } else if has("dev") || has("local") {
        Some(DbColor::Green)
    } else {
        None
    }
}

// ---- dbports ----

fn dbports_dir(home: &Path) -> PathBuf {
    std::env::var_os("DBPORTS_CONFIG")
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| home.join(".config/dbports"))
}

fn dbports(home: &Path) -> Vec<Found> {
    let dir = dbports_dir(home);
    let tunnels_path = std::env::var_os("DBPORTS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("connections.yaml"));
    let databases_path = std::env::var_os("DBPORTS_DATABASES")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("databases.yaml"));
    let tunnels = std::fs::read_to_string(tunnels_path)
        .map(|text| yaml_list(&text, "connections"))
        .unwrap_or_default();
    let databases = std::fs::read_to_string(databases_path)
        .map(|text| yaml_list(&text, "connections"))
        .unwrap_or_default();
    dbports_from(&tunnels, &databases)
}

/// dbports' tunnel entry as a [`Tunnel`] and its port.
fn dbports_tunnel(entry: &HashMap<String, Yaml>) -> Option<(u16, Tunnel)> {
    let port: u16 = entry.get("port")?.text().parse().ok()?;
    let text = |key: &str| entry.get(key).map(|v| v.text()).unwrap_or_default();
    let kind = match text("type").as_str() {
        "alloydb" => TunnelKind::AlloyDb,
        "cloudsql" => TunnelKind::CloudSql,
        "custom" => TunnelKind::Custom,
        _ => return None,
    };
    let tunnel = Tunnel {
        kind,
        instance: text("instance"),
        public_ip: text("public_ip") == "true",
        args: entry.get("args").map(Yaml::list).unwrap_or_default(),
        command: text("cmd"),
        ..Tunnel::default()
    };
    Some((port, tunnel))
}

fn dbports_from(tunnels: &[HashMap<String, Yaml>], databases: &[HashMap<String, Yaml>]) -> Vec<Found> {
    let by_name: HashMap<String, (u16, Tunnel, Vec<String>)> = tunnels
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.text();
            let (port, tunnel) = dbports_tunnel(entry)?;
            let dbs = entry.get("databases").map(Yaml::list).unwrap_or_default();
            Some((name, (port, tunnel, dbs)))
        })
        .collect();
    let mut found = Vec::new();
    let mut used = HashSet::new();
    for entry in databases {
        let text = |key: &str| entry.get(key).map(|v| v.text()).unwrap_or_default();
        let name = text("name");
        if name.is_empty() {
            continue;
        }
        let mut source = match by_name.get(&text("tunnel")) {
            Some((port, tunnel, _)) => {
                used.insert(text("tunnel"));
                tunneled(&name, *port, tunnel.clone())
            }
            None => {
                let Ok(port) = text("port").parse::<u16>() else {
                    continue;
                };
                let mut source = DataSource::new_of(
                    if is_mongo_port(port) || looks_like_mongo(&name) {
                        DbKind::MongoDb
                    } else {
                        DbKind::Postgres
                    },
                    &name,
                );
                source.host = match text("host") {
                    host if host.is_empty() => "127.0.0.1".into(),
                    host => host,
                };
                source.port = port;
                source
            }
        };
        source.database = text("database");
        source.user = text("user");
        let env = text("env");
        source.color = color_for(&env).or_else(|| color_for(&name));
        if !env.is_empty() {
            source.comment = format!("env: {env}");
        }
        source.mcp = match text("mcp").as_str() {
            "off" => McpAccess::Off,
            "write" => McpAccess::Write,
            _ => McpAccess::Read,
        };
        // dbports' own writes went through `mcp: write`; the session itself
        // follows the same choice.
        source.read_only = source.mcp != McpAccess::Write;
        let password = Some(text("password")).filter(|p| !p.is_empty());
        source.save_password = password.is_some();
        found.push(Found {
            source,
            password,
            origin: "dbports".into(),
        });
    }
    // Tunnels no connection uses: one source per database on them, without a user.
    let mut rest: Vec<_> = by_name.iter().filter(|(name, _)| !used.contains(*name)).collect();
    rest.sort_by_key(|(_, (port, _, _))| *port);
    for (name, (port, tunnel, dbs)) in rest {
        let mut source = tunneled(name, *port, tunnel.clone());
        source.database = dbs.first().cloned().unwrap_or_default();
        found.push(Found {
            source,
            password: None,
            origin: "dbports tunnel".into(),
        });
    }
    found
}

/// The few YAML shapes dbports writes: a top-level key holding a list of
/// flat maps whose values are scalars, quoted scalars or `[a, b]` lists
/// (also as `- a` block lists).
#[derive(Debug, Clone, PartialEq)]
enum Yaml {
    Scalar(String),
    List(Vec<String>),
}

impl Yaml {
    fn text(&self) -> String {
        match self {
            Self::Scalar(text) => text.clone(),
            Self::List(items) => items.join(","),
        }
    }

    fn list(&self) -> Vec<String> {
        match self {
            Self::Scalar(text) if text.is_empty() => Vec::new(),
            Self::Scalar(text) => vec![text.clone()],
            Self::List(items) => items.clone(),
        }
    }
}

/// A scalar without its comment and quotes.
fn yaml_scalar(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(other) => out.push(other),
                    None => {}
                },
                c => out.push(c),
            }
        }
        return out;
    }
    if let Some(rest) = raw.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    out.push('\'');
                    continue;
                }
                break;
            }
            out.push(c);
        }
        return out;
    }
    // A comment starts at ` #`.
    let end = raw.find(" #").unwrap_or(raw.len());
    let value = raw[..end].trim();
    if value == "~" || value == "null" {
        String::new()
    } else {
        value.to_owned()
    }
}

fn yaml_value(raw: &str) -> Yaml {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('[') {
        let inner = inner.split(']').next().unwrap_or("");
        return Yaml::List(
            inner
                .split(',')
                .map(yaml_scalar)
                .filter(|item| !item.is_empty())
                .collect(),
        );
    }
    Yaml::Scalar(yaml_scalar(raw))
}

fn yaml_list(text: &str, key: &str) -> Vec<HashMap<String, Yaml>> {
    let mut items: Vec<HashMap<String, Yaml>> = Vec::new();
    let mut inside = false;
    // The map key whose block list (`- a` lines) is being read.
    let mut block: Option<String> = None;
    let mut item_indent = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent == 0 {
            inside = trimmed.trim_end() == format!("{key}:");
            block = None;
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ").or(trimmed.strip_prefix('-').filter(|r| r.is_empty())) {
            if item_indent.is_none_or(|at| indent <= at) {
                // A new item; its first key may be on the same line.
                item_indent = Some(indent);
                items.push(HashMap::new());
                block = None;
                if let Some((k, v)) = rest.split_once(':') {
                    items
                        .last_mut()
                        .unwrap()
                        .insert(k.trim().to_owned(), yaml_value(v));
                }
            } else if let (Some(name), Some(item)) = (&block, items.last_mut()) {
                // An entry of a block list.
                match item.entry(name.clone()).or_insert(Yaml::List(Vec::new())) {
                    Yaml::List(list) => list.push(yaml_scalar(rest)),
                    other => *other = Yaml::List(vec![yaml_scalar(rest)]),
                }
            }
            continue;
        }
        let Some(item) = items.last_mut() else {
            continue;
        };
        if let Some((k, v)) = trimmed.split_once(':') {
            let value = v.trim();
            let k = k.trim().to_owned();
            if value.is_empty() {
                block = Some(k.clone());
                item.insert(k, Yaml::List(Vec::new()));
            } else {
                block = None;
                item.insert(k, yaml_value(value));
            }
        }
    }
    items
}

// ---- SOCket ----

fn socket(home: &Path) -> Vec<Found> {
    let Ok(text) = std::fs::read_to_string(home.join(".config/socradar-socket/connections.json"))
    else {
        return Vec::new();
    };
    socket_from(&text)
}

fn socket_from(text: &str) -> Vec<Found> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let string = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let number = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|n| n.as_u64().or_else(|| n.as_str().and_then(|s| s.parse().ok())))
            .and_then(|n| u16::try_from(n).ok())
    };
    for entry in root
        .get("connections")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = string(entry, "name");
        let command = string(entry, "command");
        let Some(parsed) = parse_command(&command) else {
            continue;
        };
        let Some(port) = number(entry, "port")
            .or(parsed.port)
            .or(default_port(parsed.tunnel.kind))
        else {
            continue;
        };
        let name = if name.is_empty() { parsed.name.clone() } else { name };
        found.push(Found {
            source: tunneled(&name, port, parsed.tunnel),
            password: None,
            origin: "SOCket".into(),
        });
    }
    for entry in root
        .get("forwards")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let target = string(entry, "target");
        let Some(port) = number(entry, "localPort") else {
            continue;
        };
        if target.is_empty() {
            continue;
        }
        let name = match string(entry, "name") {
            name if name.is_empty() => target.clone(),
            name => name,
        };
        let tunnel = Tunnel {
            kind: TunnelKind::Kubectl,
            instance: target,
            remote_port: number(entry, "remotePort"),
            namespace: string(entry, "namespace"),
            context: string(entry, "context"),
            ..Tunnel::default()
        };
        found.push(Found {
            source: tunneled(&name, port, tunnel),
            password: None,
            origin: "SOCket".into(),
        });
    }
    found
}

// ---- commands (SOCket, the shell, running processes) ----

/// A tunnel command taken apart.
#[derive(Debug, Clone, PartialEq)]
struct Parsed {
    tunnel: Tunnel,
    port: Option<u16>,
    /// A name from the command itself: the instance's last part, or the
    /// forwarded resource.
    name: String,
}

fn program_kind(word: &str) -> Option<TunnelKind> {
    let base = word.rsplit('/').next().unwrap_or(word);
    match base {
        "alloydb-auth-proxy" => Some(TunnelKind::AlloyDb),
        "cloud-sql-proxy" | "cloud_sql_proxy" => Some(TunnelKind::CloudSql),
        "kubectl" => Some(TunnelKind::Kubectl),
        _ => None,
    }
}

/// Finds a proxy or `kubectl port-forward` in a command line (it may sit
/// after `nohup`, `exec`, env assignments, or in a `&&` chain) and reads
/// its instance, port and arguments.
fn parse_command(command: &str) -> Option<Parsed> {
    let words = shlex::split(command).or_else(|| {
        Some(command.split_whitespace().map(str::to_owned).collect())
    })?;
    let start = words.iter().position(|w| program_kind(w).is_some())?;
    let kind = program_kind(&words[start])?;
    // The command ends where the shell would start another.
    let rest: Vec<&str> = words[start + 1..]
        .iter()
        .map(String::as_str)
        .take_while(|w| !matches!(*w, "&&" | "||" | ";" | "|" | "&" | ">" | ">>" | "2>&1"))
        .map(|w| w.trim_end_matches(['&', ';']))
        .filter(|w| !w.is_empty())
        .collect();
    match kind {
        TunnelKind::Kubectl => parse_kubectl(&rest),
        _ => parse_proxy(kind, &rest),
    }
}

fn parse_proxy(kind: TunnelKind, words: &[&str]) -> Option<Parsed> {
    let mut tunnel = Tunnel {
        kind,
        ..Tunnel::default()
    };
    let mut port = None;
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        if let Some(value) = word.strip_prefix("--port=").or(word.strip_prefix("-port=")) {
            port = value.parse().ok();
        } else if matches!(word, "--port" | "-p" | "-port") {
            port = words.get(i + 1).and_then(|v| v.parse().ok());
            i += 1;
        } else if word == "--public-ip" || word == "-public-ip" {
            tunnel.public_ip = true;
        } else if !word.starts_with('-') && tunnel.instance.is_empty() {
            // cloud_sql_proxy v1: `-instances=p:r:i=tcp:5432`.
            tunnel.instance = word.to_owned();
        } else if let Some(spec) = word.strip_prefix("-instances=") {
            let (instance, tcp) = spec.split_once("=tcp:").unwrap_or((spec, ""));
            tunnel.instance = instance.to_owned();
            port = port.or_else(|| tcp.rsplit(':').next().and_then(|p| p.parse().ok()));
        } else {
            tunnel.args.push(word.to_owned());
            // A flag's separate value goes with it.
            if let Some(next) = words.get(i + 1).filter(|next| {
                !word.contains('=') && !next.starts_with('-') && !tunnel.instance.is_empty()
            }) {
                tunnel.args.push((*next).to_owned());
                i += 1;
            }
        }
        i += 1;
    }
    // cloud-sql-proxy v2 also takes `instance?port=N`.
    if let Some((instance, query)) = tunnel.instance.clone().split_once('?') {
        tunnel.instance = instance.to_owned();
        if port.is_none() {
            port = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("port="))
                .and_then(|p| p.parse().ok());
        }
    }
    if tunnel.instance.is_empty() || tunnel.instance.contains('$') {
        return None;
    }
    let name = tunnel
        .instance
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(&tunnel.instance)
        .to_owned();
    Some(Parsed { tunnel, port, name })
}

fn parse_kubectl(words: &[&str]) -> Option<Parsed> {
    let at = words.iter().position(|w| *w == "port-forward")?;
    let mut tunnel = Tunnel {
        kind: TunnelKind::Kubectl,
        ..Tunnel::default()
    };
    let mut port = None;
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        let value = |flag: &str| {
            word.strip_prefix(&format!("{flag}="))
                .map(str::to_owned)
                .or_else(|| (word == flag).then(|| words.get(i + 1).map(|v| (*v).to_owned())).flatten())
        };
        if i == at {
        } else if let Some(ns) = value("-n").or_else(|| value("--namespace")) {
            tunnel.namespace = ns;
            i += usize::from(!word.contains('='));
        } else if let Some(context) = value("--context") {
            tunnel.context = context;
            i += usize::from(!word.contains('='));
        } else if word.starts_with('-') {
            tunnel.args.push(word.to_owned());
        } else if i > at && tunnel.instance.is_empty() {
            tunnel.instance = word.to_owned();
        } else if i > at && port.is_none() {
            // `5432`, `5433:5432`, `:5432` (any local port; skipped).
            let (local, remote) = word.split_once(':').unwrap_or((word, word));
            port = local.parse().ok();
            tunnel.remote_port = remote.parse().ok();
        }
        i += 1;
    }
    if tunnel.instance.is_empty() || tunnel.instance.contains('$') {
        return None;
    }
    if let Some(rest) = tunnel.instance.strip_prefix("service/") {
        tunnel.instance = format!("svc/{rest}");
    }
    if tunnel.remote_port == port {
        tunnel.remote_port = None;
    }
    let name = tunnel
        .instance
        .rsplit('/')
        .next()
        .unwrap_or(&tunnel.instance)
        .to_owned();
    Some(Parsed { tunnel, port, name })
}

/// `password=`, `token=`, `--token`…: a line not to copy anywhere.
fn secret_like(command: &str) -> bool {
    let lower = command.to_lowercase();
    lower.contains("--token")
        || ["password", "passwd", "secret", "api_key", "api-key", "apikey", "token"]
            .iter()
            .any(|word| {
                lower.match_indices(word).any(|(at, _)| {
                    lower[at + word.len()..].trim_start().starts_with('=')
                })
            })
}

/// A proxy started without `--port` listens on 5432.
fn default_port(kind: TunnelKind) -> Option<u16> {
    (kind != TunnelKind::Kubectl).then_some(5432)
}

/// Aliases and functions in a shell start-up file that open a tunnel. An
/// alias is named after itself; a function after its name; a bare line
/// after its instance.
fn shell_file(text: &str, origin: &str) -> Vec<Found> {
    let mut found = Vec::new();
    let mut function: Option<String> = None;
    let mut depth = 0i32;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        // `name() {`, `function name {`, `function name() {`
        let opens = line
            .strip_prefix("function ")
            .map(|rest| rest.split(['(', ' ', '{']).next().unwrap_or("").to_owned())
            .or_else(|| {
                line.split_once("()")
                    .filter(|(name, _)| {
                        !name.is_empty()
                            && name
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || "_-.:".contains(c))
                    })
                    .map(|(name, _)| name.to_owned())
            });
        if let Some(name) = opens.filter(|_| depth == 0) {
            function = Some(name);
        }
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        let (name, command) = match line.strip_prefix("alias ") {
            Some(alias) => match alias.split_once('=') {
                Some((name, value)) => {
                    let value = shlex::split(value)
                        .and_then(|parts| parts.into_iter().next())
                        .unwrap_or_else(|| value.trim_matches(['\'', '"']).to_owned());
                    (Some(name.trim().to_owned()), value)
                }
                None => continue,
            },
            None => (function.clone(), line.to_owned()),
        };
        if depth <= 0 {
            function = None;
            depth = 0;
        }
        // A line that holds a secret is left alone, as SOCket leaves it.
        if secret_like(&command) {
            continue;
        }
        let Some(parsed) = parse_command(&command) else {
            continue;
        };
        let Some(port) = parsed.port.or(default_port(parsed.tunnel.kind)) else {
            continue;
        };
        let name = name.unwrap_or_else(|| parsed.name.clone());
        found.push(Found {
            source: tunneled(&name, port, parsed.tunnel),
            password: None,
            origin: origin.to_owned(),
        });
    }
    found
}

/// Proxies and port-forwards running now, with the port each listens on.
fn running() -> Vec<Found> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-axww", "-o", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    let listeners = super::tunnel::listeners();
    let ports_by_pid: HashMap<u32, Vec<u16>> =
        listeners
            .iter()
            .fold(HashMap::new(), |mut map, (port, listener)| {
                map.entry(listener.pid).or_default().push(*port);
                map
            });
    running_from(&String::from_utf8_lossy(&output.stdout), &ports_by_pid)
}

fn running_from(ps: &str, ports_by_pid: &HashMap<u32, Vec<u16>>) -> Vec<Found> {
    let mut found = Vec::new();
    for line in ps.lines() {
        let line = line.trim();
        let Some((pid, command)) = line.split_once(' ') else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        let Some(parsed) = parse_command(command) else {
            continue;
        };
        let port = parsed.port.or_else(|| {
            ports_by_pid
                .get(&pid)
                .and_then(|ports| ports.iter().min().copied())
        });
        let Some(port) = port else {
            continue;
        };
        found.push(Found {
            source: tunneled(&parsed.name, port, parsed.tunnel),
            password: None,
            origin: "running".into(),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_commands_come_apart() {
        let p = parse_command(
            "alloydb-auth-proxy projects/p/locations/r/clusters/c/instances/main --public-ip --port=5400",
        )
        .unwrap();
        assert_eq!(p.tunnel.kind, TunnelKind::AlloyDb);
        assert_eq!(p.tunnel.instance, "projects/p/locations/r/clusters/c/instances/main");
        assert!(p.tunnel.public_ip);
        assert_eq!(p.port, Some(5400));
        assert_eq!(p.name, "main");

        let p = parse_command("nohup cloud-sql-proxy p:europe-west4:pg --port 5502 --auto-iam-authn > /tmp/x.log 2>&1 &").unwrap();
        assert_eq!(p.tunnel.kind, TunnelKind::CloudSql);
        assert_eq!(p.tunnel.instance, "p:europe-west4:pg");
        assert_eq!(p.port, Some(5502));
        assert_eq!(p.tunnel.args, vec!["--auto-iam-authn"]);

        let p = parse_command("cloud_sql_proxy -instances=p:r:i=tcp:5433").unwrap();
        assert_eq!((p.tunnel.instance.as_str(), p.port), ("p:r:i", Some(5433)));

        let p = parse_command("/opt/homebrew/bin/cloud-sql-proxy 'p:r:i?port=6000'").unwrap();
        assert_eq!((p.tunnel.instance.as_str(), p.port), ("p:r:i", Some(6000)));

        assert!(parse_command("echo hello").is_none());
        assert!(parse_command("cloud-sql-proxy $INSTANCE --port 1").is_none());
    }

    #[test]
    fn kubectl_port_forwards_come_apart() {
        let p = parse_command("kubectl -n data port-forward svc/mongo 27018:27017 --context prod").unwrap();
        assert_eq!(p.tunnel.kind, TunnelKind::Kubectl);
        assert_eq!(p.tunnel.instance, "svc/mongo");
        assert_eq!(p.tunnel.namespace, "data");
        assert_eq!(p.tunnel.context, "prod");
        assert_eq!(p.port, Some(27018));
        assert_eq!(p.tunnel.remote_port, Some(27017));
        let p = parse_command("kubectl port-forward service/pg 5432").unwrap();
        assert_eq!(p.tunnel.instance, "svc/pg");
        let p = parse_command("kubectl port-forward pod/pg-0 5432").unwrap();
        assert_eq!((p.port, p.tunnel.remote_port), (Some(5432), None));
        assert!(parse_command("kubectl get pods").is_none());
    }

    #[test]
    fn shell_aliases_and_functions_are_found() {
        let rc = r#"
# a comment: cloud-sql-proxy x:y:z --port 1
alias pg-prod='cloud-sql-proxy proj:europe-west4:prod --port 5432'
alias ll='ls -la'
mongo_fw() {
  kubectl -n db port-forward svc/mongodb 27017:27017
}
function alloy {
  alloydb-auth-proxy projects/p/locations/r/clusters/c/instances/i --port=5400 &
}
"#;
        let found = shell_file(rc, "~/.zshrc");
        let names: Vec<_> = found.iter().map(|f| f.source.name.as_str()).collect();
        assert_eq!(names, ["pg-prod", "mongo_fw", "alloy"]);
        assert_eq!(found[0].source.port, 5432);
        assert_eq!(found[0].source.color, Some(DbColor::Red));
        assert_eq!(found[1].source.kind, DbKind::MongoDb);
        assert_eq!(found[1].source.port, 27017);
        assert_eq!(found[2].source.tunnel.as_ref().unwrap().kind, TunnelKind::AlloyDb);
        assert!(found.iter().all(|f| f.origin == "~/.zshrc"));
        // A secret on the line, or a variable as the instance: left out.
        let rc = "alias a='PGPASSWORD=x cloud-sql-proxy p:r:i --port 1'\nalias b='cloud-sql-proxy $I --port 2'\nalias c='cloud-sql-proxy p:r:c'";
        let found = shell_file(rc, "~/.zshrc");
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].source.name.as_str(), found[0].source.port), ("c", 5432));
    }

    #[test]
    fn sourced_files_under_home_are_followed() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("db.sh"), "alias pg='cloud-sql-proxy p:r:i --port 6000'").unwrap();
        std::fs::create_dir_all(home.path().join(".oh-my-zsh")).unwrap();
        std::fs::write(home.path().join(".oh-my-zsh/x.sh"), "").unwrap();
        let rc = "source ~/db.sh\n. $HOME/.oh-my-zsh/x.sh\nsource /etc/profile\nsource ~/missing.sh";
        let files = sourced_files(rc, home.path());
        assert_eq!(files, [home.path().join("db.sh")]);
    }

    #[test]
    fn socket_connections_and_forwards_are_found() {
        let json = r#"{
          "connections": [
            {"name": "prod-db", "port": 5400, "command": "alloydb-auth-proxy projects/p/locations/r/clusters/c/instances/i --public-ip --port=5400"},
            {"name": "broken", "port": 1, "command": "echo"}
          ],
          "forwards": [{"name": "api", "target": "svc/mongo", "localPort": 27017, "remotePort": 27017}]
        }"#;
        let found = socket_from(json);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].source.name, "prod-db");
        assert_eq!(found[0].source.host, "127.0.0.1");
        assert_eq!(found[1].source.kind, DbKind::MongoDb);
        assert_eq!(found[1].source.tunnel.as_ref().unwrap().kind, TunnelKind::Kubectl);
    }

    #[test]
    fn dbports_files_become_sources_with_their_users() {
        let tunnels = yaml_list(
            r#"
# header
connections:
  - name: prod            # primary
    group: alloydb
    type: alloydb
    instance: projects/p/locations/r/clusters/c/instances/main
    port: 5400
    public_ip: true
    databases: [socradar, "other"]
  - name: pg2
    type: cloudsql
    instance: "p:r:pg2"
    port: 5502
    args:
      - --auto-iam-authn
    databases:
      - app
"#,
            "connections",
        );
        assert_eq!(tunnels.len(), 2);
        assert_eq!(
            tunnels[0]["databases"],
            Yaml::List(vec!["socradar".into(), "other".into()])
        );
        assert_eq!(tunnels[1]["args"], Yaml::List(vec!["--auto-iam-authn".into()]));
        let databases = yaml_list(
            r#"connections:
  - name: socradar@prod
    tunnel: prod
    database: socradar
    user: ahmet
    password: 'it''s # secret'
    env: prod
    mcp: write
  - name: local
    host: 127.0.0.1
    port: 5432
    database: app
    user: dev
"#,
            "connections",
        );
        let found = dbports_from(&tunnels, &databases);
        let names: Vec<_> = found.iter().map(|f| f.source.name.as_str()).collect();
        assert_eq!(names, ["socradar@prod", "local", "pg2"]);
        let prod = &found[0];
        assert_eq!(prod.password.as_deref(), Some("it's # secret"));
        assert_eq!(prod.source.port, 5400);
        assert_eq!(prod.source.user, "ahmet");
        assert_eq!(prod.source.mcp, McpAccess::Write);
        assert_eq!(prod.source.color, Some(DbColor::Red));
        assert!(prod.source.tunnel.as_ref().unwrap().public_ip);
        assert!(found[1].source.tunnel.is_none());
        assert_eq!(found[2].source.database, "app");
        assert_eq!(found[2].source.tunnel.as_ref().unwrap().args, ["--auto-iam-authn"]);
    }

    #[test]
    fn known_connections_and_bare_duplicates_are_left_out() {
        let mut saved = DataSource::new("prod");
        saved.port = 5400;
        saved.database = "socradar".into();
        saved.user = "ahmet".into();
        let tunnel = Tunnel {
            kind: TunnelKind::AlloyDb,
            instance: "projects/x".into(),
            ..Tunnel::default()
        };
        let bare = |name: &str, port| Found {
            source: tunneled(name, port, tunnel.clone()),
            password: None,
            origin: "x".into(),
        };
        let mut described = bare("pg2@app", 5502);
        described.source.database = "app".into();
        let found = vec![
            bare("alias-prod", 5400), // the saved source's port
            described,
            bare("socket-pg2", 5502), // the described one's port
            bare("new", 5600),
            bare("new-again", 5600),
        ];
        let mut other = bare("other-instance", 5600);
        other.source.tunnel.as_mut().unwrap().instance = "projects/y".into();
        let mut found = found;
        found.push(other);
        let kept: Vec<_> = dedupe(found, &[saved])
            .into_iter()
            .map(|f| f.source.name)
            .collect();
        assert_eq!(kept, ["pg2@app", "new", "other-instance"]);
    }

    #[test]
    fn running_proxies_take_their_port_from_the_listener() {
        let ps = "  123 /usr/local/bin/cloud-sql-proxy p:r:i\n  9 /bin/zsh\n 77 kubectl port-forward svc/a 8080:80\n";
        let ports = HashMap::from([(123u32, vec![5555u16])]);
        let found = running_from(ps, &ports);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].source.port, 5555);
        assert_eq!(found[1].source.port, 8080);
    }
}
