//! What an agent can do with a project's API collections: list them, read a
//! request, run one and import a cURL command. Every action works on the
//! files under `<project>/.monocode/api/`, as the API view does, so the view
//! picks changes up from disk.

use super::collection::{Collection, ItemPath};
use super::env::Environment;
use super::exchange::{run, ExchangeInput};
use super::model::{ApiRequest, Auth, Body};
use super::send::SendOptions;
use super::{api_dir, COLLECTION_SUFFIX, ENVIRONMENT_SUFFIX};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// How much of a response body an agent is given.
const BODY_CHARS: usize = 24_000;

pub fn is_api_action(action: &str) -> bool {
    matches!(
        action,
        "api.list" | "api.read" | "api.run" | "api.import_curl"
    )
}

/// Actions that only read.
pub fn is_read_only(action: &str) -> bool {
    matches!(action, "api.list" | "api.read")
}

struct Loaded {
    file: PathBuf,
    collection: Collection,
}

fn stem(file: &Path, suffix: &str) -> String {
    file.file_name()
        .map(|n| n.to_string_lossy().trim_end_matches(suffix).to_owned())
        .unwrap_or_default()
}

fn collections(project: &str) -> Vec<Loaded> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(api_dir(project))
        .map(|dir| dir.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    files
        .into_iter()
        .filter(|f| f.to_string_lossy().ends_with(COLLECTION_SUFFIX))
        .filter_map(|file| {
            Collection::load(&file)
                .ok()
                .map(|collection| Loaded { file, collection })
        })
        .collect()
}

fn environments(project: &str) -> Vec<(PathBuf, Environment)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(api_dir(project))
        .map(|dir| dir.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    files
        .into_iter()
        .filter(|f| f.to_string_lossy().ends_with(ENVIRONMENT_SUFFIX))
        .filter_map(|file| Environment::load(&file).ok().map(|e| (file, e)))
        .collect()
}

fn pick_collection(all: Vec<Loaded>, wanted: Option<&str>) -> Result<Loaded, String> {
    let names = |all: &[Loaded]| {
        all.iter()
            .map(|c| c.collection.name())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match wanted {
        Some(wanted) => {
            let lower = wanted.to_lowercase();
            let mut hits: Vec<Loaded> = all
                .into_iter()
                .filter(|c| {
                    c.collection.name().to_lowercase() == lower
                        || stem(&c.file, COLLECTION_SUFFIX).to_lowercase() == lower
                })
                .collect();
            match hits.len() {
                1 => Ok(hits.remove(0)),
                0 => Err(format!(
                    "No collection named {wanted:?}. Use api.list to see them."
                )),
                _ => Err(format!("More than one collection is named {wanted:?}.")),
            }
        }
        None => match all.len() {
            0 => Err("This project has no API collections.".into()),
            1 => Ok(all.into_iter().next().expect("one collection")),
            _ => Err(format!(
                "Name the collection with \"collection\": one of {}.",
                names(&all)
            )),
        },
    }
}

/// A request by name (`Login`), by folder path (`Auth/Login`) or by index
/// path (`[0, 2]`).
fn pick_request(collection: &Collection, wanted: &Value) -> Result<ItemPath, String> {
    if let Some(indexes) = wanted.as_array() {
        let path: Option<ItemPath> = indexes
            .iter()
            .map(|v| v.as_u64().map(|n| n as usize))
            .collect();
        return path
            .filter(|p| collection.request(p).is_some())
            .ok_or_else(|| "No request at that index path.".to_owned());
    }
    let Some(wanted) = wanted.as_str() else {
        return Err("Provide \"request\": a name, a Folder/Name path or an index path.".into());
    };
    let lower = wanted.trim().to_lowercase();
    let entries = collection.entries();
    // The full path of each request: its folders and its name.
    let named: Vec<(ItemPath, String, String)> = entries
        .iter()
        .filter(|e| !e.folder)
        .map(|e| {
            let mut parts: Vec<String> = (1..e.path.len())
                .filter_map(|d| collection.item_name(&e.path[..d]))
                .collect();
            parts.push(e.name.clone());
            (
                e.path.clone(),
                e.name.to_lowercase(),
                parts.join("/").to_lowercase(),
            )
        })
        .collect();
    let by_path: Vec<&(ItemPath, String, String)> = named.iter().filter(|n| n.2 == lower).collect();
    if by_path.len() == 1 {
        return Ok(by_path[0].0.clone());
    }
    let by_name: Vec<&(ItemPath, String, String)> = named.iter().filter(|n| n.1 == lower).collect();
    match by_name.len() {
        1 => Ok(by_name[0].0.clone()),
        0 => Err(format!(
            "No request named {wanted:?}. Use api.list to see them."
        )),
        _ => Err(format!(
            "More than one request is named {wanted:?}; use a Folder/Name path: {}.",
            by_name
                .iter()
                .map(|n| n.2.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn clip(text: &str) -> (String, bool) {
    if text.chars().count() <= BODY_CHARS {
        return (text.to_owned(), false);
    }
    (text.chars().take(BODY_CHARS).collect(), true)
}

fn request_json(request: &ApiRequest) -> Value {
    let rows = |rows: &[super::model::KeyValue]| {
        rows.iter()
            .map(|r| json!({"key": r.key, "value": r.value, "enabled": !r.disabled}))
            .collect::<Vec<_>>()
    };
    let body = match &request.body {
        Body::None => json!({"mode": "none"}),
        Body::Raw { text, language } => {
            json!({"mode": "raw", "language": language.id(), "text": clip(text).0})
        }
        Body::UrlEncoded(fields) => json!({"mode": "urlencoded", "fields": rows(fields)}),
        Body::FormData(parts) => json!({
            "mode": "form-data",
            "fields": parts.iter().map(|p| json!({
                "key": p.key, "value": p.value, "file": p.kind == super::model::PartKind::File,
                "enabled": !p.disabled,
            })).collect::<Vec<_>>(),
        }),
        Body::Unsupported(mode) => json!({"mode": mode, "supported": false}),
    };
    // Credentials are not echoed back; their presence is.
    let auth = match &request.auth {
        Auth::Inherit => json!({"type": "inherit"}),
        Auth::None => json!({"type": "none"}),
        Auth::Basic { username, .. } => {
            json!({"type": "basic", "username": username, "password": "[set]"})
        }
        Auth::Bearer { .. } => json!({"type": "bearer", "token": "[set]"}),
        Auth::ApiKey { key, in_header, .. } => {
            json!({"type": "apikey", "key": key, "in": if *in_header { "header" } else { "query" }, "value": "[set]"})
        }
        Auth::Unsupported(kind) => json!({"type": kind, "supported": false}),
    };
    json!({
        "name": request.name,
        "method": request.method,
        "url": request.url_text(),
        "headers": rows(&request.headers),
        "body": body,
        "auth": auth,
        "preRequestScript": request.pre_request,
        "testScript": request.tests,
        "description": request.description,
    })
}

fn text_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

pub fn action(project: &str, action: &str, input: &Value) -> Result<Value, String> {
    match action {
        "api.list" => Ok(list(project)),
        "api.read" => read(project, input),
        "api.run" => run_request(project, input),
        "api.import_curl" => import_curl(project, input),
        _ => Err("Unknown API action".into()),
    }
}

fn list(project: &str) -> Value {
    let collections: Vec<Value> = collections(project)
        .iter()
        .map(|c| {
            let entries = c.collection.entries();
            let requests: Vec<Value> = entries
                .iter()
                .filter(|e| !e.folder)
                .map(|e| {
                    let folders: Vec<String> = (1..e.path.len())
                        .filter_map(|d| c.collection.item_name(&e.path[..d]))
                        .collect();
                    let url = c
                        .collection
                        .request(&e.path)
                        .map(|r| r.url_text())
                        .unwrap_or_default();
                    json!({
                        "name": e.name,
                        "method": e.method,
                        "url": url,
                        "folder": folders.join("/"),
                        "path": e.path,
                    })
                })
                .collect();
            json!({
                "name": c.collection.name(),
                "file": stem(&c.file, COLLECTION_SUFFIX),
                "variables": c.collection.own_variables().iter().map(|v| v.key.clone()).collect::<Vec<_>>(),
                "requests": requests,
            })
        })
        .collect();
    let envs: Vec<Value> = environments(project)
        .iter()
        .map(|(file, e)| {
            json!({
                "name": e.name(),
                "file": stem(file, ENVIRONMENT_SUFFIX),
                "variables": e.values().iter().filter(|v| v.enabled).map(|v| v.key.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({"collections": collections, "environments": envs})
}

fn read(project: &str, input: &Value) -> Result<Value, String> {
    let loaded = pick_collection(collections(project), text_field(input, "collection"))?;
    let path = pick_request(
        &loaded.collection,
        input.get("request").unwrap_or(&Value::Null),
    )?;
    let request = loaded
        .collection
        .request(&path)
        .ok_or("The request is gone")?;
    Ok(
        json!({"collection": loaded.collection.name(), "path": path, "request": request_json(&request)}),
    )
}

fn pick_environment(
    project: &str,
    wanted: Option<&str>,
) -> Result<Option<(PathBuf, Environment)>, String> {
    let mut all = environments(project);
    match wanted {
        Some(wanted) if wanted.eq_ignore_ascii_case("none") => Ok(None),
        Some(wanted) => {
            let lower = wanted.to_lowercase();
            let at = all.iter().position(|(file, e)| {
                e.name().to_lowercase() == lower
                    || stem(file, ENVIRONMENT_SUFFIX).to_lowercase() == lower
            });
            match at {
                Some(at) => Ok(Some(all.remove(at))),
                None => Err(format!(
                    "No environment named {wanted:?}. Use api.list to see them."
                )),
            }
        }
        // Without a name, a lone environment is the one meant.
        None if all.len() == 1 => Ok(Some(all.remove(0))),
        None => Ok(None),
    }
}

fn run_request(project: &str, input: &Value) -> Result<Value, String> {
    let mut loaded = pick_collection(collections(project), text_field(input, "collection"))?;
    let path = pick_request(
        &loaded.collection,
        input.get("request").unwrap_or(&Value::Null),
    )?;
    let request = loaded
        .collection
        .request(&path)
        .ok_or("The request is gone")?;
    let mut environment = pick_environment(project, text_field(input, "environment"))?;
    let timeout = input
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .map_or(30, |s| s.clamp(1, 120));

    let mut pre: Vec<String> = loaded
        .collection
        .scripts_above(&path, "prerequest")
        .into_iter()
        .map(|(_, script)| script)
        .collect();
    pre.push(request.pre_request.clone());
    let mut tests: Vec<String> = loaded
        .collection
        .scripts_above(&path, "test")
        .into_iter()
        .map(|(_, script)| script)
        .collect();
    tests.push(request.tests.clone());
    let output = run(ExchangeInput {
        inherited_auth: loaded.collection.inherited_auth(&path),
        collection_vars: loaded.collection.variables(&path),
        environment_name: environment
            .as_ref()
            .map(|(_, e)| e.name())
            .unwrap_or_default(),
        environment_vars: environment
            .as_ref()
            .map(|(_, e)| e.variables())
            .unwrap_or_default(),
        globals: Vec::new(),
        pre_scripts: pre,
        test_scripts: tests,
        options: SendOptions {
            timeout: std::time::Duration::from_secs(timeout),
            ..SendOptions::default()
        },
        request,
    });

    // What the scripts set is saved, as it is when the view sends.
    let mut saved: Vec<String> = Vec::new();
    if let (false, Some((file, env))) = (output.environment.is_empty(), environment.as_mut()) {
        env.apply(&output.environment);
        if env.save().is_ok() {
            saved.push(format!("environment {}", stem(file, ENVIRONMENT_SUFFIX)));
        }
    }
    if !output.collection.is_empty() {
        loaded.collection.apply_variables(&output.collection);
        if loaded.collection.save().is_ok() {
            saved.push(format!("collection {}", loaded.collection.name()));
        }
    }
    let variables_set: Vec<Value> = output
        .environment
        .iter()
        .chain(&output.collection)
        .map(|c| json!({"key": c.key, "removed": c.value.is_none()}))
        .collect();
    let test_json: Vec<Value> = output
        .tests
        .iter()
        .map(
            |t| json!({"name": t.name, "passed": t.passed, "error": t.error, "skipped": t.skipped}),
        )
        .collect();
    let mut result = json!({
        "request": output.request.name,
        "tests": test_json,
        "passedTests": output.tests.iter().filter(|t| t.passed).count(),
        "logs": output.logs,
        "variablesSet": variables_set,
        "savedTo": saved,
    });
    match output.outcome {
        Ok(response) => {
            let (body, clipped) = clip(&response.text());
            let headers: Vec<Value> = response
                .headers
                .iter()
                .map(|(k, v)| json!({"name": k, "value": v}))
                .collect();
            result["response"] = json!({
                "status": response.status,
                "reason": response.reason,
                "ms": response.elapsed.as_millis() as u64,
                "bytes": response.body.len(),
                "headers": headers,
                "body": body,
                "bodyClipped": clipped || response.truncated,
                "url": response.url,
            });
        }
        Err(error) => result["error"] = json!(error.to_string()),
    }
    Ok(result)
}

fn import_curl(project: &str, input: &Value) -> Result<Value, String> {
    let command = text_field(input, "curl").ok_or("Provide \"curl\": the command to import.")?;
    let import = super::curl::parse_curl(command).map_err(|e| e.to_string())?;
    let mut loaded = match pick_collection(collections(project), text_field(input, "collection")) {
        Ok(loaded) => loaded,
        // A project with no collection gets one named after the project.
        Err(_) if collections(project).is_empty() && text_field(input, "collection").is_none() => {
            let name = Path::new(project)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "API".into());
            let file = api_dir(project).join(format!("api{COLLECTION_SUFFIX}"));
            let mut collection = Collection::new(&name);
            collection.set_path(file.clone());
            Loaded { file, collection }
        }
        Err(error) => return Err(error),
    };
    let mut parent: ItemPath = Vec::new();
    if let Some(folder) = text_field(input, "folder") {
        for part in folder.split('/').filter(|p| !p.is_empty()) {
            let existing = loaded.collection.entries().into_iter().find(|e| {
                e.folder
                    && e.name == part
                    && e.path.len() == parent.len() + 1
                    && e.path.starts_with(&parent)
            });
            parent = match existing {
                Some(entry) => entry.path,
                None => loaded
                    .collection
                    .add_folder(&parent, part)
                    .ok_or("Could not make the folder")?,
            };
        }
    }
    let name = text_field(input, "name")
        .map(str::to_owned)
        .unwrap_or_else(|| import.request.name.clone());
    let path = loaded
        .collection
        .add_request(&parent, &name)
        .ok_or("Could not add the request")?;
    let mut request = import.request;
    request.name = name.clone();
    loaded.collection.set_request(&path, &request);
    loaded.collection.save().map_err(|e| e.to_string())?;
    Ok(json!({
        "collection": loaded.collection.name(),
        "request": name,
        "path": path,
        "warnings": import.warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOP: &str = r#"{
	"info": { "name": "Shop", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json" },
	"item": [
		{ "name": "Auth", "item": [
			{ "name": "Login", "request": { "method": "POST", "url": "http://placeholder.invalid/login" } }
		] },
		{ "name": "Login", "request": { "method": "GET", "url": "http://placeholder.invalid/other" } },
		{ "name": "Ping", "request": { "method": "GET", "url": "{{base}}/ping", "auth": { "type": "bearer", "bearer": [ { "key": "token", "value": "s3cret" } ] } } },
		{ "name": "Admin", "item": [ { "name": "Audit", "request": { "method": "GET", "url": "http://placeholder.invalid/a" } } ] },
		{ "name": "Reports", "item": [ { "name": "Audit", "request": { "method": "GET", "url": "http://placeholder.invalid/r" } } ] }
	]
}"#;

    fn project() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let api = api_dir(&root);
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("shop.postman_collection.json"), SHOP).unwrap();
        (dir, root)
    }

    #[test]
    fn the_listing_shows_collections_requests_and_environments() {
        let (_dir, root) = project();
        std::fs::write(
            api_dir(&root).join("local.postman_environment.json"),
            r#"{"name":"Local","values":[{"key":"base","value":"http://x","enabled":true}]}"#,
        )
        .unwrap();
        let out = action(&root, "api.list", &json!({})).unwrap();
        let requests = out["collections"][0]["requests"].as_array().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[0]["folder"], "Auth");
        assert_eq!(requests[0]["method"], "POST");
        assert_eq!(out["environments"][0]["name"], "Local");
        assert_eq!(out["environments"][0]["variables"][0], "base");
    }

    #[test]
    fn requests_are_found_by_name_by_path_or_by_index_and_ambiguity_is_explained() {
        let (_dir, root) = project();
        let read = |request: Value| action(&root, "api.read", &json!({"request": request}));
        assert_eq!(read(json!("Ping")).unwrap()["request"]["method"], "GET");
        assert_eq!(
            read(json!("auth/login")).unwrap()["request"]["method"],
            "POST"
        );
        assert_eq!(read(json!([1])).unwrap()["request"]["name"], "Login");
        // A top-level name beats the same name inside a folder.
        assert_eq!(read(json!("Login")).unwrap()["request"]["method"], "GET");
        let error = read(json!("Audit")).unwrap_err();
        assert!(
            error.contains("admin/audit") && error.contains("reports/audit"),
            "{error}"
        );
        assert_eq!(
            read(json!("reports/audit")).unwrap()["request"]["url"],
            "http://placeholder.invalid/r"
        );
        assert!(read(json!("nope")).unwrap_err().contains("api.list"));
        assert!(read(json!([9])).is_err());
        // Credentials are not echoed.
        let ping = read(json!("Ping")).unwrap();
        assert_eq!(ping["request"]["auth"]["token"], "[set]");
        assert!(!ping.to_string().contains("s3cret"));
    }

    #[test]
    fn running_a_request_returns_the_response_and_saves_what_scripts_set() {
        let (_dir, root) = project();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{\"token\":\"z\"}")
                .unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        std::fs::write(
            api_dir(&root).join("local.postman_environment.json"),
            format!(
                r#"{{"name":"Local","values":[{{"key":"base","value":"{url}","enabled":true}}]}}"#
            ),
        )
        .unwrap();
        // Give Ping a test script that stores the token.
        let file = api_dir(&root).join("shop.postman_collection.json");
        let mut collection = Collection::load(&file).unwrap();
        let mut ping = collection.request(&[2]).unwrap();
        ping.tests = "pm.test('ok', () => pm.response.to.have.status(200)); pm.environment.set('token', pm.response.json().token);".into();
        collection.set_request(&[2], &ping);
        collection.save().unwrap();

        let out = action(&root, "api.run", &json!({"request": "Ping"})).unwrap();
        assert_eq!(out["response"]["status"], 200, "{out}");
        assert_eq!(out["response"]["body"], "{\"token\":\"z\"}");
        assert_eq!(out["passedTests"], 1);
        assert_eq!(out["variablesSet"][0]["key"], "token");
        assert!(out["savedTo"][0].as_str().unwrap().contains("local"));
        let seen = server.join().unwrap();
        assert!(seen.starts_with("GET /ping HTTP/1.1"), "{seen}");
        assert!(seen.contains("Authorization: Bearer s3cret"));
        let env =
            Environment::load(&api_dir(&root).join("local.postman_environment.json")).unwrap();
        assert!(env
            .values()
            .iter()
            .any(|v| v.key == "token" && v.value == "z"));
    }

    #[test]
    fn a_failed_run_says_why_instead_of_returning_a_response() {
        let (_dir, root) = project();
        let out = action(&root, "api.run", &json!({"request": "Ping"})).unwrap();
        assert!(out["error"].as_str().unwrap().contains("{{base}}"), "{out}");
        assert!(out.get("response").is_none());
        let unknown = action(
            &root,
            "api.run",
            &json!({"request": "Ping", "environment": "Nope"}),
        );
        assert!(unknown.unwrap_err().contains("No environment"));
    }

    #[test]
    fn a_curl_command_becomes_a_request_in_a_folder() {
        let (_dir, root) = project();
        let out = action(
            &root,
            "api.import_curl",
            &json!({"curl": "curl -X POST https://x.test/v1/users -H 'Content-Type: application/json' -d '{\"a\":1}'", "folder": "Users/Admin", "name": "Create"}),
        )
        .unwrap();
        assert_eq!(out["request"], "Create");
        let collection =
            Collection::load(&api_dir(&root).join("shop.postman_collection.json")).unwrap();
        let path: Vec<usize> = serde_json::from_value(out["path"].clone()).unwrap();
        let request = collection.request(&path).unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "https://x.test/v1/users");
        assert_eq!(collection.item_name(&path[..2]).as_deref(), Some("Admin"));
        assert!(action(&root, "api.import_curl", &json!({"curl": "wget x"})).is_err());
    }

    #[test]
    fn a_project_without_collections_gets_one_for_an_import() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        assert!(action(&root, "api.read", &json!({"request": "x"}))
            .unwrap_err()
            .contains("no API collections"));
        let out = action(
            &root,
            "api.import_curl",
            &json!({"curl": "curl https://x.test/ping"}),
        )
        .unwrap();
        assert_eq!(out["request"], "GET ping");
        assert_eq!(
            action(&root, "api.list", &json!({})).unwrap()["collections"][0]["requests"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
