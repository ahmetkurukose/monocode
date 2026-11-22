//! Files in and out of pages (K50, K51): PDFs, uploads, downloads. They
//! stay in the project's folder: uploads read only files inside it, PDFs
//! and downloads (and saved screenshots) write only inside it. A path is
//! judged after symlinks resolve, so a link out of the project is refused.

use std::{
    path::{Component, Path, PathBuf},
    pin::pin,
};

use base64::Engine as _;
use futures_util::future::{Either, select};
use serde_json::{Value, json};

use super::{Ctx, Reply, act, effects, invalid, nav, quote};
use crate::browser::{CdpError, ToolOutput};

/// Where `browser_download` saves by default, in the project.
pub const DOWNLOADS: &str = "downloads";
const DOWNLOAD_MS: u64 = 60_000;
const MAX_DOWNLOAD_MS: u64 = 300_000;

fn refused(message: impl std::fmt::Display) -> ToolOutput {
    ToolOutput::error("browser_path_refused", message)
}

/// The project folder, symlinks resolved.
fn root(project: &str) -> Result<PathBuf, ToolOutput> {
    std::fs::canonicalize(project)
        .map_err(|e| invalid(format!("the project folder {project} is not readable: {e}")))
}

fn outside(raw: &str, root: &Path) -> ToolOutput {
    refused(format!(
        "{raw} is outside the project folder {}; files go only there",
        root.display()
    ))
}

/// `raw` (relative to the project, or absolute) with its existing part's
/// symlinks resolved; the rest must not climb with `..`.
fn resolve(root: &Path, raw: &str) -> Result<PathBuf, ToolOutput> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(invalid("the path is empty"));
    }
    let given = Path::new(raw);
    let path = if given.is_absolute() {
        given.to_path_buf()
    } else {
        root.join(given)
    };
    // The deepest part that exists, resolved; then what is to be made.
    let mut existing = path.as_path();
    let mut rest = Vec::new();
    while std::fs::symlink_metadata(existing).is_err() {
        let Some(name) = existing.file_name() else {
            return Err(outside(raw, root));
        };
        rest.push(name.to_owned());
        existing = existing.parent().ok_or_else(|| outside(raw, root))?;
    }
    let mut out = std::fs::canonicalize(existing).map_err(|e| invalid(format!("{raw}: {e}")))?;
    for name in rest.iter().rev() {
        match Path::new(name).components().next() {
            Some(Component::Normal(part)) => out.push(part),
            Some(Component::CurDir) => {}
            _ => return Err(outside(raw, root)),
        }
    }
    // `..` the walk up kept (`a/missing/..`) is not followed.
    if out.components().any(|c| c == Component::ParentDir) {
        return Err(outside(raw, root));
    }
    if !out.starts_with(root) {
        return Err(outside(raw, root));
    }
    Ok(out)
}

/// A file to write inside the project; its folders are made.
pub fn writable(project: &str, raw: &str) -> Result<PathBuf, ToolOutput> {
    let root = root(project)?;
    let path = resolve(&root, raw)?;
    if path == root || path.is_dir() {
        return Err(invalid(format!("{raw} is a folder; give a file path")));
    }
    let parent = path.parent().ok_or_else(|| outside(raw, &root))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| invalid(format!("could not make {}: {e}", parent.display())))?;
    // Made now: a link put there meanwhile still counts.
    let parent = std::fs::canonicalize(parent).map_err(|e| invalid(format!("{raw}: {e}")))?;
    if !parent.starts_with(&root) {
        return Err(outside(raw, &root));
    }
    Ok(parent.join(path.file_name().ok_or_else(|| outside(raw, &root))?))
}

/// A folder inside the project, made if missing.
pub fn folder(project: &str, raw: &str) -> Result<PathBuf, ToolOutput> {
    let root = root(project)?;
    let path = resolve(&root, raw)?;
    std::fs::create_dir_all(&path)
        .map_err(|e| invalid(format!("could not make {}: {e}", path.display())))?;
    let path = std::fs::canonicalize(&path).map_err(|e| invalid(format!("{raw}: {e}")))?;
    if !path.starts_with(&root) {
        return Err(outside(raw, &root));
    }
    if !path.is_dir() {
        return Err(invalid(format!("{raw} is not a folder")));
    }
    Ok(path)
}

/// A file inside the project to read.
pub fn readable(project: &str, raw: &str) -> Result<PathBuf, ToolOutput> {
    let root = root(project)?;
    let raw = raw.trim();
    let given = Path::new(raw);
    let path = if given.is_absolute() {
        given.to_path_buf()
    } else {
        root.join(given)
    };
    let path =
        std::fs::canonicalize(&path).map_err(|_| invalid(format!("there is no file {raw}")))?;
    if !path.starts_with(&root) {
        return Err(outside(raw, &root));
    }
    if !path.is_file() {
        return Err(invalid(format!("{raw} is not a file")));
    }
    Ok(path)
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1e3),
        _ => format!("{:.1} MB", bytes as f64 / 1e6),
    }
}

fn flag(args: &Value, name: &str) -> bool {
    args.get(name).and_then(Value::as_bool).unwrap_or(false)
}

pub async fn pdf(ctx: &Ctx, args: &Value) -> Reply {
    let Some(raw) = args.get("path").and_then(Value::as_str) else {
        return Err(invalid("path is required"));
    };
    let path = writable(&ctx.scope.project, raw)?;
    let printed = ctx
        .call(
            "Page.printToPDF",
            json!({
                "landscape": flag(args, "landscape"),
                "printBackground": flag(args, "background"),
                "preferCSSPageSize": true,
            }),
        )
        .await?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(printed["data"].as_str().unwrap_or(""))
        .map_err(|e| invalid(format!("the PDF was not readable: {e}")))?;
    if data.is_empty() {
        return Err(invalid("Chromium returned an empty PDF"));
    }
    std::fs::write(&path, &data)
        .map_err(|e| invalid(format!("could not write {}: {e}", path.display())))?;
    Ok(ToolOutput::text(format!(
        "Saved page {} as a PDF ({}) to {}.",
        ctx.id,
        size(data.len() as u64),
        path.display()
    )))
}

/// The file input of an element: itself, its label's control, one inside
/// it, or the only one of its form.
const FILE_INPUT: &str = r#"function() {
  const isFile = el => el && el.tagName === 'INPUT' && el.type === 'file';
  if (isFile(this)) return this;
  if (this.tagName === 'LABEL' && isFile(this.control)) return this.control;
  const inside = this.querySelector && this.querySelector('input[type=file]');
  if (inside) return inside;
  const near = this.closest && this.closest('form, dialog, [role=dialog]');
  const all = (near || document).querySelectorAll('input[type=file]');
  return all.length === 1 ? all[0] : null;
}"#;

pub async fn upload(ctx: &Ctx, args: &Value) -> Reply {
    let Some(r) = act::ref_arg(args) else {
        return Err(invalid("ref is required"));
    };
    let raws: Vec<&str> = match args.get("paths") {
        Some(Value::Array(paths)) => paths.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(path)) => vec![path.as_str()],
        _ => Vec::new(),
    };
    if raws.is_empty() {
        return Err(invalid("paths is required: the files to choose"));
    }
    let files = raws
        .iter()
        .map(|raw| readable(&ctx.scope.project, raw))
        .collect::<Result<Vec<_>, _>>()?;
    let entry = act::resolve(ctx, &r).await?;
    let what = act::label(&r, &entry);
    let session = entry.session.as_deref();
    let object = ctx
        .call_in(
            session,
            "DOM.resolveNode",
            json!({"backendNodeId": entry.backend, "objectGroup": "monocode"}),
        )
        .await?;
    let object = object["object"]["objectId"].as_str().unwrap_or_default();
    let input = ctx
        .call_in(
            session,
            "Runtime.callFunctionOn",
            json!({"objectId": object, "functionDeclaration": FILE_INPUT}),
        )
        .await?;
    let Some(input) = input["result"]["objectId"].as_str() else {
        return Err(invalid(format!(
            "{what} is not a file input and has none near it; pass the ref of the file input or its button"
        )));
    };
    if files.len() > 1 {
        let multiple = ctx
            .call_in(
                session,
                "Runtime.callFunctionOn",
                json!({"objectId": input, "functionDeclaration": "function() { return this.multiple; }", "returnByValue": true}),
            )
            .await?;
        if multiple["result"]["value"] == false {
            return Err(invalid(format!(
                "{what} takes one file, not {}",
                files.len()
            )));
        }
    }
    let node = ctx
        .call_in(session, "DOM.describeNode", json!({"objectId": input}))
        .await?;
    let Some(backend) = node["node"]["backendNodeId"].as_i64() else {
        return Err(invalid(format!("{what}'s file input is gone")));
    };
    let before = effects::before(ctx).await?;
    let paths: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    ctx.call_in(
        session,
        "DOM.setFileInputFiles",
        json!({"files": paths, "backendNodeId": backend}),
    )
    .await?;
    let names: Vec<String> = files
        .iter()
        .map(|p| quote(&p.file_name().unwrap_or_default().to_string_lossy(), 100))
        .collect();
    let effects = effects::settle(ctx, before).await?;
    Ok(effects.with(format!("Chose {} for {what}.", names.join(", ")), true))
}

/// `browser_download`'s time.
pub fn download_ms(args: &Value) -> u64 {
    args.get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DOWNLOAD_MS)
        .clamp(1_000, MAX_DOWNLOAD_MS)
}

/// Fetches a URL like a link with a `download` attribute.
fn fetch_script(url: &str) -> String {
    format!(
        "(() => {{ const a = document.createElement('a'); a.href = {}; a.download = ''; \
a.rel = 'noopener'; a.style.display = 'none'; (document.body || document.documentElement).appendChild(a); \
a.click(); a.remove(); return true; }})()",
        json!(url)
    )
}

pub async fn download(ctx: &Ctx, args: &Value) -> Reply {
    let budget = download_ms(args);
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .filter(|u| !u.trim().is_empty());
    let r = act::ref_arg(args);
    if url.is_some() == r.is_some() {
        return Err(invalid("give ref or url, one of them"));
    }
    let raw_dir = args
        .get("dir")
        .and_then(Value::as_str)
        .filter(|d| !d.trim().is_empty())
        .unwrap_or(DOWNLOADS);
    let dir = folder(&ctx.scope.project, raw_dir)?;
    let url = url.map(nav::normalize).transpose()?;
    let point = match &r {
        Some(_) => Some(act::target(ctx, args).await?),
        None => None,
    };
    let finished = ctx.page.expect_download(dir.clone());
    let from = match (point, url) {
        (Some((x, y, what)), _) => {
            act::press_at(ctx, x, y, "left", false).await?;
            format!("clicking {what}")
        }
        (None, Some(url)) => {
            let answer = ctx
                .act_raw(
                    "Runtime.evaluate",
                    json!({"expression": fetch_script(&url), "userGesture": true, "returnByValue": true}),
                )
                .await?;
            match answer {
                Ok(_) => {}
                Err(CdpError::Gone) => return Err(super::page_gone(ctx.id)),
                Err(error) => return Err(ctx.failure("Runtime.evaluate", error)),
            }
            url
        }
        (None, None) => unreachable!("ref or url was checked"),
    };
    let waited = match select(pin!(finished), pin!(ctx.sleep(budget))).await {
        Either::Left((done, _)) => done,
        Either::Right(_) => {
            return Err(ToolOutput::error(
                "browser_timeout",
                format!(
                    "no download from {from} finished in {} s; it may still be going, into {}",
                    budget / 1000,
                    dir.display()
                ),
            ));
        }
    };
    let path = waited.map_err(|e| {
        ToolOutput::error(
            "browser_download_failed",
            format!("the download from {from} failed: {e}"),
        )
    })?;
    let path = std::fs::canonicalize(&path).map_err(|e| {
        ToolOutput::error(
            "browser_download_failed",
            format!("the downloaded file {} is not there: {e}", path.display()),
        )
    })?;
    if !path.starts_with(&dir) {
        return Err(refused(format!(
            "the download landed at {}, outside {}",
            path.display(),
            dir.display()
        )));
    }
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(ToolOutput::text(format!(
        "Downloaded {} ({}) from {from}.",
        path.display(),
        size(bytes)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(result: Result<PathBuf, ToolOutput>) -> String {
        match result {
            Ok(path) => format!("ok {}", path.display()),
            Err(e) => super::super::fake::text(&e)
                .split(':')
                .next()
                .unwrap_or("")
                .to_owned(),
        }
    }

    #[test]
    fn paths_stay_in_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let project = base.join("project");
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(project.join("docs")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(project.join("docs/a.txt"), "a").unwrap();
        std::fs::write(elsewhere.join("secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(&elsewhere, project.join("out")).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("secret.txt"), project.join("link.txt")).unwrap();
        let p = project.to_str().unwrap();

        // Writing: relative, absolute, new folders.
        assert_eq!(writable(p, "out.pdf").unwrap(), project.join("out.pdf"));
        assert_eq!(
            writable(p, "new/deeper/x.pdf").unwrap(),
            project.join("new/deeper/x.pdf")
        );
        assert!(project.join("new/deeper").is_dir());
        let absolute = project.join("docs/b.pdf");
        assert_eq!(writable(p, absolute.to_str().unwrap()).unwrap(), absolute);
        assert_eq!(writable(p, "docs/../c.pdf").unwrap(), project.join("c.pdf"));
        for escape in [
            "../x.pdf",
            "docs/../../x.pdf",
            "missing/../../x.pdf",
            "out/x.pdf",
            "out/new/x.pdf",
            "/etc/x.pdf",
        ] {
            assert_eq!(
                code(writable(p, escape)),
                "browser_path_refused",
                "{escape}"
            );
        }
        assert!(!elsewhere.join("new").exists());
        assert!(writable(p, "docs").is_err());

        // Folders.
        assert_eq!(folder(p, "downloads").unwrap(), project.join("downloads"));
        assert_eq!(code(folder(p, "out")), "browser_path_refused");
        assert_eq!(code(folder(p, "..")), "browser_path_refused");

        // Reading.
        assert_eq!(
            readable(p, "docs/a.txt").unwrap(),
            project.join("docs/a.txt")
        );
        for escape in ["link.txt", "out/secret.txt", "../elsewhere/secret.txt"] {
            assert_eq!(
                code(readable(p, escape)),
                "browser_path_refused",
                "{escape}"
            );
        }
        assert!(readable(p, "docs/none.txt").is_err());
        assert!(readable(p, "docs").is_err());
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(size(12), "12 B");
        assert_eq!(size(34_500), "34.5 KB");
        assert_eq!(size(2_000_000), "2.0 MB");
    }
}
