//! A Postman Collection v2.1 file, edited in place.
//!
//! The parsed JSON is the document. Reading builds an [`ApiRequest`] view of
//! one item; writing merges that view back into the same item, so everything
//! this client does not understand (scripts, saved responses, descriptions,
//! `protocolProfileBehavior`, ...) is saved exactly as it was found, in the
//! same key order.

use super::model::{
    path_var_names, ApiRequest, Auth, Body, FormPart, KeyValue, PartKind, RawLanguage,
};
use super::ApiError;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "https://schema.getpostman.com/json/collection/v2.1.0/collection.json";

/// Where an item sits: the index in the collection's `item` list, then the
/// index in that folder's `item` list, and so on. Empty is the collection.
pub type ItemPath = Vec<usize>;

/// One row of the flattened collection tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: ItemPath,
    pub depth: usize,
    pub name: String,
    pub folder: bool,
    /// The HTTP method of a request.
    pub method: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Collection {
    doc: Value,
    path: Option<PathBuf>,
}

impl Collection {
    pub fn new(name: &str) -> Self {
        Self {
            doc: json!({
                "info": {
                    "_postman_id": uuid::Uuid::new_v4().to_string(),
                    "name": name,
                    "schema": SCHEMA,
                },
                "item": [],
            }),
            path: None,
        }
    }

    pub fn parse(text: &str) -> Result<Self, ApiError> {
        let doc: Value = serde_json::from_str(text)
            .map_err(|e| ApiError::new(format!("This is not valid JSON: {e}")))?;
        if !doc.is_object() || doc.get("info").is_none_or(|info| !info.is_object()) {
            return Err(ApiError::new(
                "This is not a Postman collection: it has no \"info\" section.",
            ));
        }
        if let Some(schema) = doc.pointer("/info/schema").and_then(Value::as_str) {
            if !schema.contains("/collection/v2") {
                return Err(ApiError::new(format!(
                    "Only Postman Collection v2.0 and v2.1 files are supported (this one is {schema})."
                )));
            }
        }
        Ok(Self { doc, path: None })
    }

    pub fn load(path: &Path) -> Result<Self, ApiError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ApiError::new(format!("Could not read {}: {e}", path.display())))?;
        let mut collection = Self::parse(&text)?;
        collection.path = Some(path.to_owned());
        Ok(collection)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    /// The file as Postman writes it: tab-indented.
    pub fn to_json(&self) -> String {
        super::pretty_tabs(&self.doc)
    }

    /// Writes the file through a sibling temp file, so a crash never leaves
    /// half a collection behind.
    pub fn save(&self) -> Result<(), ApiError> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| ApiError::new("This collection has no file yet."))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| ApiError::new(format!("Could not create {}: {e}", dir.display())))?;
        }
        let mut temp = path.clone().into_os_string();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        let mut text = self.to_json();
        text.push('\n');
        std::fs::write(&temp, text)
            .and_then(|_| std::fs::rename(&temp, path))
            .map_err(|e| ApiError::new(format!("Could not save {}: {e}", path.display())))
    }

    pub fn name(&self) -> String {
        self.doc.pointer("/info/name").map(text).unwrap_or_default()
    }

    pub fn set_name(&mut self, name: &str) {
        if let Some(info) = self.doc.get_mut("info").and_then(Value::as_object_mut) {
            info.insert("name".into(), name.into());
        }
    }

    pub fn description(&self) -> String {
        self.doc
            .pointer("/info/description")
            .map(description_text)
            .unwrap_or_default()
    }

    // ----- tree -----

    fn node(&self, path: &[usize]) -> Option<&Value> {
        let mut node = &self.doc;
        for index in path {
            node = node.get("item")?.get(*index)?;
        }
        Some(node)
    }

    fn node_mut(&mut self, path: &[usize]) -> Option<&mut Value> {
        let mut node = &mut self.doc;
        for index in path {
            node = node.get_mut("item")?.get_mut(*index)?;
        }
        Some(node)
    }

    fn items_mut(&mut self, parent: &[usize]) -> Option<&mut Vec<Value>> {
        let node = self.node_mut(parent)?;
        let object = node.as_object_mut()?;
        let items = object
            .entry("item")
            .or_insert_with(|| Value::Array(Vec::new()));
        items.as_array_mut()
    }

    pub fn is_folder(&self, path: &[usize]) -> bool {
        path.is_empty() || self.node(path).is_some_and(|n| n.get("item").is_some())
    }

    pub fn item_name(&self, path: &[usize]) -> Option<String> {
        if path.is_empty() {
            return Some(self.name());
        }
        self.node(path)
            .map(|n| n.get("name").map(text).unwrap_or_default())
    }

    /// The whole tree, depth first, folders before their contents.
    pub fn entries(&self) -> Vec<TreeEntry> {
        let mut out = Vec::new();
        fn walk(items: Option<&Value>, prefix: &mut Vec<usize>, out: &mut Vec<TreeEntry>) {
            let Some(items) = items.and_then(Value::as_array) else {
                return;
            };
            for (index, item) in items.iter().enumerate() {
                prefix.push(index);
                let folder = item.get("item").is_some();
                out.push(TreeEntry {
                    path: prefix.clone(),
                    depth: prefix.len() - 1,
                    name: item.get("name").map(text).unwrap_or_default(),
                    folder,
                    method: (!folder).then(|| request_method(item)),
                });
                if folder {
                    walk(item.get("item"), prefix, out);
                }
                prefix.pop();
            }
        }
        walk(self.doc.get("item"), &mut Vec::new(), &mut out);
        out
    }

    /// How many requests the collection holds.
    pub fn request_count(&self) -> usize {
        self.entries().iter().filter(|e| !e.folder).count()
    }

    // ----- editing the tree -----

    pub fn add_request(&mut self, parent: &[usize], name: &str) -> Option<ItemPath> {
        let item = json!({
            "name": name,
            "request": { "method": "GET", "header": [], "url": { "raw": "" } },
            "response": [],
        });
        self.push_item(parent, item)
    }

    pub fn add_folder(&mut self, parent: &[usize], name: &str) -> Option<ItemPath> {
        self.push_item(parent, json!({ "name": name, "item": [] }))
    }

    fn push_item(&mut self, parent: &[usize], item: Value) -> Option<ItemPath> {
        if !self.is_folder(parent) {
            return None;
        }
        let items = self.items_mut(parent)?;
        items.push(item);
        let mut path = parent.to_vec();
        path.push(items.len() - 1);
        Some(path)
    }

    pub fn rename(&mut self, path: &[usize], name: &str) {
        if path.is_empty() {
            self.set_name(name);
        } else if let Some(map) = self.node_mut(path).and_then(Value::as_object_mut) {
            map.insert("name".into(), name.into());
        }
    }

    pub fn remove(&mut self, path: &[usize]) -> bool {
        let Some((last, parent)) = path.split_last() else {
            return false;
        };
        match self.items_mut(parent) {
            Some(items) if *last < items.len() => {
                items.remove(*last);
                true
            }
            _ => false,
        }
    }

    /// Copies an item (and a folder's contents) right after itself.
    pub fn duplicate(&mut self, path: &[usize]) -> Option<ItemPath> {
        let (last, parent) = path.split_last()?;
        let mut copy = self.node(path)?.clone();
        if let Some(map) = copy.as_object_mut() {
            let name = map.get("name").map(text).unwrap_or_default();
            map.insert("name".into(), format!("{name} copy").into());
            map.remove("id");
        }
        let items = self.items_mut(parent)?;
        items.insert(last + 1, copy);
        let mut new_path = parent.to_vec();
        new_path.push(last + 1);
        Some(new_path)
    }

    /// Moves an item to `index` inside `parent`. Fails when the target is
    /// the item itself or inside it.
    pub fn move_item(
        &mut self,
        path: &[usize],
        parent: &[usize],
        index: usize,
    ) -> Option<ItemPath> {
        let (last, old_parent) = path.split_last()?;
        if parent.len() >= path.len() && parent[..path.len()] == *path {
            return None;
        }
        if !self.is_folder(parent) {
            return None;
        }
        let item = self.node(path)?.clone();
        self.items_mut(old_parent)?.remove(*last);
        // Removing shifted later siblings left, which can move the parent too.
        let mut parent = parent.to_vec();
        if old_parent.len() < parent.len()
            && parent[..old_parent.len()] == *old_parent
            && parent[old_parent.len()] > *last
        {
            parent[old_parent.len()] -= 1;
        }
        let items = self.items_mut(&parent)?;
        let index = index.min(items.len());
        items.insert(index, item);
        parent.push(index);
        Some(parent)
    }

    // ----- requests -----

    pub fn request(&self, path: &[usize]) -> Option<ApiRequest> {
        let item = self.node(path)?;
        if path.is_empty() || item.get("item").is_some() {
            return None;
        }
        Some(read_request(item))
    }

    pub fn set_request(&mut self, path: &[usize], request: &ApiRequest) -> bool {
        if path.is_empty() {
            return false;
        }
        match self.node_mut(path) {
            Some(item) if item.is_object() && item.get("item").is_none() => {
                write_request(item, request);
                true
            }
            _ => false,
        }
    }

    // ----- scripts -----

    /// The `listen` scripts that run before a request's own: the
    /// collection's, then each folder's on the way down, with the name of
    /// the item each belongs to.
    pub fn scripts_above(&self, path: &[usize], listen: &str) -> Vec<(String, String)> {
        (0..path.len())
            .filter_map(|depth| {
                let node = self.node(&path[..depth])?;
                let script = read_script(node, listen);
                (!script.trim().is_empty()).then(|| {
                    let name = if depth == 0 {
                        self.name()
                    } else {
                        node.get("name").map(text).unwrap_or_default()
                    };
                    (name, script)
                })
            })
            .collect()
    }

    // ----- inheritance -----

    /// The auth a request actually sends: its own, else the nearest folder's,
    /// else the collection's.
    pub fn effective_auth(&self, path: &[usize]) -> Auth {
        match self
            .node(path)
            .and_then(|n| n.get("request"))
            .and_then(|r| r.get("auth"))
            .filter(|a| !a.is_null())
        {
            Some(own) => read_auth(Some(own)),
            None => self.inherited_auth(path),
        }
    }

    /// The auth a request at `path` gets from its folders and collection,
    /// ignoring any of its own.
    pub fn inherited_auth(&self, path: &[usize]) -> Auth {
        for depth in (0..path.len()).rev() {
            if let Some(auth) = self
                .node(&path[..depth])
                .and_then(|n| n.get("auth"))
                .filter(|a| !a.is_null())
            {
                return read_auth(Some(auth));
            }
        }
        Auth::None
    }

    /// Collection variables, then each folder on the way down; an inner
    /// folder's value wins over an outer one.
    pub fn variables(&self, path: &[usize]) -> Vec<KeyValue> {
        let mut out: Vec<KeyValue> = Vec::new();
        for depth in 0..=path.len() {
            let Some(node) = self.node(&path[..depth]) else {
                continue;
            };
            for row in read_rows(node.get("variable")) {
                if row.disabled || row.key.is_empty() {
                    continue;
                }
                match out.iter_mut().find(|known| known.key == row.key) {
                    Some(known) => known.value = row.value,
                    None => out.push(row),
                }
            }
        }
        out
    }

    /// Replaces the collection's own variables.
    pub fn set_variables(&mut self, rows: &[KeyValue]) {
        if let Some(map) = self.doc.as_object_mut() {
            let merged = merge_rows(map.get("variable"), rows, false);
            if rows.is_empty() && !map.contains_key("variable") {
                return;
            }
            map.insert("variable".into(), merged);
        }
    }

    /// Applies what a script set or removed to the collection's own variables.
    pub fn apply_variables(&mut self, changes: &[super::exchange::VarChange]) {
        let mut rows = self.own_variables();
        for change in changes {
            match &change.value {
                Some(value) => match rows.iter_mut().find(|r| r.key == change.key) {
                    Some(row) => {
                        row.value = value.clone();
                        row.disabled = false;
                    }
                    None => rows.push(KeyValue::new(change.key.clone(), value.clone())),
                },
                None => rows.retain(|r| r.key != change.key),
            }
        }
        self.set_variables(&rows);
    }

    /// The collection's own variables, disabled ones too.
    pub fn own_variables(&self) -> Vec<KeyValue> {
        read_rows(self.doc.get("variable"))
    }

    /// The collection-level auth, for editing.
    pub fn auth(&self) -> Auth {
        match self.doc.get("auth").filter(|a| !a.is_null()) {
            Some(auth) => read_auth(Some(auth)),
            None => Auth::Inherit,
        }
    }

    pub fn set_auth(&mut self, auth: &Auth) {
        if let Some(map) = self.doc.as_object_mut() {
            write_auth(map, auth);
        }
    }
}

// ----- reading -----

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn description_text(value: &Value) -> String {
    match value {
        Value::Object(map) => map.get("content").map(text).unwrap_or_default(),
        other => text(other),
    }
}

fn read_rows(value: Option<&Value>) -> Vec<KeyValue> {
    value
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| KeyValue {
                    key: row.get("key").map(text).unwrap_or_default(),
                    value: row.get("value").map(text).unwrap_or_default(),
                    disabled: row
                        .get("disabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn request_method(item: &Value) -> String {
    item.get("request")
        .and_then(|r| r.get("method"))
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .unwrap_or("GET")
        .to_uppercase()
}

fn read_request(item: &Value) -> ApiRequest {
    let mut request = ApiRequest {
        name: item.get("name").map(text).unwrap_or_default(),
        method: request_method(item),
        ..ApiRequest::default()
    };
    request.pre_request = read_script(item, "prerequest");
    request.tests = read_script(item, "test");
    let Some(source) = item.get("request") else {
        return request;
    };
    if let Some(raw) = source.as_str() {
        request.set_url_text(raw);
        return request;
    }
    let url = source.get("url");
    match url {
        Some(Value::String(raw)) => request.set_url_text(raw),
        Some(Value::Object(object)) => {
            let raw = object.get("raw").and_then(Value::as_str).unwrap_or("");
            let base = if raw.is_empty() {
                rebuild_url(object)
            } else {
                raw.split('?').next().unwrap_or("").to_owned()
            };
            request.url = base;
            request.query = match object.get("query") {
                Some(rows) if rows.is_array() => read_rows(Some(rows)),
                _ => raw
                    .split_once('?')
                    .map(|(_, query)| {
                        query
                            .split('&')
                            .filter(|p| !p.is_empty())
                            .map(|pair| {
                                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                                KeyValue::new(k, v)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            request.path_vars = read_rows(object.get("variable"));
        }
        _ => {}
    }
    // Rows for every `:name` in the path, whether or not the file lists them.
    let known = request.path_vars.clone();
    request.path_vars = path_var_names(&request.url)
        .into_iter()
        .map(|name| {
            known
                .iter()
                .find(|row| row.key == name)
                .cloned()
                .unwrap_or_else(|| KeyValue::new(name, ""))
        })
        .collect();
    request.headers = read_rows(source.get("header"));
    request.body = read_body(source.get("body"));
    request.auth = match source.get("auth").filter(|a| !a.is_null()) {
        Some(auth) => read_auth(Some(auth)),
        None => Auth::Inherit,
    };
    request.description = source
        .get("description")
        .map(description_text)
        .unwrap_or_default();
    request
}

/// The script of the first `listen` event on a node (`prerequest` or `test`).
fn read_script(node: &Value, listen: &str) -> String {
    node.get("event")
        .and_then(Value::as_array)
        .and_then(|events| {
            events
                .iter()
                .find(|e| e.get("listen").and_then(Value::as_str) == Some(listen))
        })
        .and_then(|event| event.pointer("/script/exec"))
        .map(|exec| match exec {
            Value::Array(lines) => lines.iter().map(text).collect::<Vec<_>>().join("\n"),
            other => text(other),
        })
        .unwrap_or_default()
}

/// Sets the `listen` script of an item. An unchanged script is left as the
/// file has it, and an emptied one removes its event.
fn write_script(item: &mut Map<String, Value>, listen: &str, script: &str) {
    let existing = read_script(&Value::Object(item.clone()), listen);
    if existing == script {
        return;
    }
    let events = item
        .entry("event")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(list) = events.as_array_mut() else {
        return;
    };
    let at = list
        .iter()
        .position(|e| e.get("listen").and_then(Value::as_str) == Some(listen));
    let lines = || json!(script.split('\n').collect::<Vec<_>>());
    match at {
        Some(index) if script.trim().is_empty() => {
            list.remove(index);
        }
        Some(index) => {
            let event = as_map(&mut list[index]);
            let target = as_map(
                event
                    .entry("script")
                    .or_insert_with(|| Value::Object(Map::new())),
            );
            target.insert("exec".into(), lines());
            target
                .entry("type")
                .or_insert_with(|| "text/javascript".into());
        }
        None if script.trim().is_empty() => {}
        None => list.push(json!({
            "listen": listen,
            "script": { "type": "text/javascript", "exec": lines() },
        })),
    }
    if list.is_empty() {
        item.remove("event");
    }
}

/// Builds `protocol://host:port/path` from a URL object that has no `raw`.
fn rebuild_url(url: &Map<String, Value>) -> String {
    let join = |value: Option<&Value>, sep: &str| match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| match part {
                Value::Object(o) => o.get("value").map(text).unwrap_or_default(),
                other => text(other),
            })
            .collect::<Vec<_>>()
            .join(sep),
        _ => String::new(),
    };
    let mut out = String::new();
    match url.get("protocol").and_then(Value::as_str) {
        Some(protocol) if !protocol.is_empty() => {
            out.push_str(protocol);
            out.push_str("://");
        }
        _ => {}
    }
    out.push_str(&join(url.get("host"), "."));
    if let Some(port) = url.get("port").map(text).filter(|p| !p.is_empty()) {
        out.push(':');
        out.push_str(&port);
    }
    let path = join(url.get("path"), "/");
    if !path.is_empty() {
        out.push('/');
        out.push_str(path.trim_start_matches('/'));
    }
    out
}

fn read_body(body: Option<&Value>) -> Body {
    let Some(body) = body.filter(|b| b.is_object()) else {
        return Body::None;
    };
    match body.get("mode").and_then(Value::as_str) {
        None | Some("") => Body::None,
        Some("raw") => Body::Raw {
            text: body.get("raw").map(text).unwrap_or_default(),
            language: RawLanguage::from_id(
                body.pointer("/options/raw/language")
                    .and_then(Value::as_str)
                    .unwrap_or("text"),
            ),
        },
        Some("urlencoded") => Body::UrlEncoded(read_rows(body.get("urlencoded"))),
        Some("formdata") => Body::FormData(
            body.get("formdata")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .map(|row| {
                            let file = row.get("type").and_then(Value::as_str) == Some("file");
                            FormPart {
                                key: row.get("key").map(text).unwrap_or_default(),
                                value: if file {
                                    row.get("src").map(text).unwrap_or_default()
                                } else {
                                    row.get("value").map(text).unwrap_or_default()
                                },
                                kind: if file { PartKind::File } else { PartKind::Text },
                                content_type: row.get("contentType").map(text).unwrap_or_default(),
                                disabled: row
                                    .get("disabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        Some(other) => Body::Unsupported(other.to_owned()),
    }
}

/// Auth parameters are a list of `{key, value}` in v2.1 and a plain object
/// in v2.0; both are read.
fn auth_params(auth: &Value, kind: &str) -> Vec<(String, String)> {
    match auth.get(kind) {
        Some(Value::Array(rows)) => rows
            .iter()
            .map(|row| {
                (
                    row.get("key").map(text).unwrap_or_default(),
                    row.get("value").map(text).unwrap_or_default(),
                )
            })
            .collect(),
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), text(v))).collect(),
        _ => Vec::new(),
    }
}

fn read_auth(auth: Option<&Value>) -> Auth {
    let Some(auth) = auth else {
        return Auth::Inherit;
    };
    let kind = auth.get("type").and_then(Value::as_str).unwrap_or("");
    let params = auth_params(auth, kind);
    let get = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    };
    match kind {
        "noauth" | "" => Auth::None,
        "basic" => Auth::Basic {
            username: get("username"),
            password: get("password"),
        },
        "bearer" => Auth::Bearer {
            token: get("token"),
        },
        "apikey" => Auth::ApiKey {
            key: get("key"),
            value: get("value"),
            in_header: get("in") != "query",
        },
        other => Auth::Unsupported(other.to_owned()),
    }
}

// ----- writing -----

fn as_map(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    match value {
        Value::Object(map) => map,
        _ => unreachable!("just made an object"),
    }
}

/// Rows written back over the ones in the file: a row keeps the object it
/// came from (descriptions, types) when its key is still there.
fn merge_rows(existing: Option<&Value>, rows: &[KeyValue], typed: bool) -> Value {
    let old: Vec<&Value> = existing
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let mut used = vec![false; old.len()];
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let found = old.iter().enumerate().find(|(index, value)| {
            !used[*index] && value.get("key").map(text).as_deref() == Some(row.key.as_str())
        });
        let mut object = match found {
            Some((index, value)) => {
                used[index] = true;
                (*value).clone()
            }
            None => {
                let mut fresh = Map::new();
                fresh.insert("key".into(), Value::Null);
                fresh.insert("value".into(), Value::Null);
                if typed {
                    fresh.insert("type".into(), "text".into());
                }
                Value::Object(fresh)
            }
        };
        let map = as_map(&mut object);
        map.insert("key".into(), row.key.clone().into());
        map.insert("value".into(), row.value.clone().into());
        if row.disabled {
            map.insert("disabled".into(), true.into());
        } else {
            map.remove("disabled");
        }
        out.push(object);
    }
    Value::Array(out)
}

fn write_request(item: &mut Value, request: &ApiRequest) {
    let item = as_map(item);
    item.insert("name".into(), request.name.clone().into());
    write_script(item, "prerequest", &request.pre_request);
    write_script(item, "test", &request.tests);
    let source = item
        .entry("request")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::String(raw) = source {
        let raw = raw.clone();
        *source = json!({ "url": raw });
    }
    let source = as_map(source);
    source.insert("method".into(), request.method.to_uppercase().into());
    write_url(source, request);
    let headers = merge_rows(source.get("header"), &request.headers, true);
    source.insert("header".into(), headers);
    write_body(source, &request.body);
    write_auth(source, &request.auth);
    if request.description.is_empty() && !source.contains_key("description") {
        // Nothing to write.
    } else {
        match source.get_mut("description") {
            Some(Value::Object(map)) => {
                map.insert("content".into(), request.description.clone().into());
            }
            _ if request.description.is_empty() => {
                source.remove("description");
            }
            _ => {
                source.insert("description".into(), request.description.clone().into());
            }
        }
    }
}

/// Splits `protocol://host:port/path` into Postman's structured fields.
fn split_url(base: &str) -> (Option<String>, Vec<String>, Option<String>, Vec<String>) {
    let (protocol, rest) = match base.split_once("://") {
        Some((protocol, rest)) => (Some(protocol.to_owned()), rest),
        None => (None, base),
    };
    let (authority, path) = match rest.find('/') {
        Some(slash) => (&rest[..slash], &rest[slash + 1..]),
        None => (rest, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit())
                && !host.ends_with('}') =>
        {
            (host, Some(port.to_owned()))
        }
        _ => (authority, None),
    };
    let host: Vec<String> = if host.contains("{{") || host.starts_with('[') {
        vec![host.to_owned()]
    } else {
        host.split('.').map(str::to_owned).collect()
    };
    let path: Vec<String> = if path.is_empty() {
        Vec::new()
    } else {
        path.split('/').map(str::to_owned).collect()
    };
    (protocol, host, port, path)
}

fn write_url(source: &mut Map<String, Value>, request: &ApiRequest) {
    let url = source
        .entry("url")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::String(raw) = url {
        let raw = raw.clone();
        *url = json!({ "raw": raw });
    }
    let url = as_map(url);
    url.insert("raw".into(), request.url_text().into());
    let (base, hash) = match request.url.split_once('#') {
        Some((base, hash)) => (base, Some(hash)),
        None => (request.url.as_str(), None),
    };
    let (protocol, host, port, path) = split_url(base);
    let set_or_remove = |url: &mut Map<String, Value>, key: &str, value: Option<Value>| match value
    {
        Some(value) => {
            url.insert(key.into(), value);
        }
        None => {
            url.remove(key);
        }
    };
    set_or_remove(url, "protocol", protocol.map(Value::from));
    set_or_remove(url, "host", Some(json!(host)));
    set_or_remove(url, "port", port.map(Value::from));
    set_or_remove(url, "path", (!path.is_empty()).then(|| json!(path)));
    set_or_remove(url, "hash", hash.map(Value::from));
    let query = merge_rows(url.get("query"), &request.query, false);
    set_or_remove(
        url,
        "query",
        query
            .as_array()
            .is_some_and(|q| !q.is_empty())
            .then_some(query),
    );
    let variables = merge_rows(url.get("variable"), &request.path_vars, false);
    set_or_remove(
        url,
        "variable",
        variables
            .as_array()
            .is_some_and(|v| !v.is_empty())
            .then_some(variables),
    );
}

fn write_body(source: &mut Map<String, Value>, body: &Body) {
    match body {
        Body::Unsupported(_) => {}
        Body::None => {
            source.remove("body");
        }
        Body::Raw { text, language } => {
            let target = as_map(
                source
                    .entry("body")
                    .or_insert_with(|| Value::Object(Map::new())),
            );
            target.insert("mode".into(), "raw".into());
            target.insert("raw".into(), text.clone().into());
            let options = as_map(
                target
                    .entry("options")
                    .or_insert_with(|| Value::Object(Map::new())),
            );
            let raw_options = as_map(
                options
                    .entry("raw")
                    .or_insert_with(|| Value::Object(Map::new())),
            );
            raw_options.insert("language".into(), language.id().into());
        }
        Body::UrlEncoded(rows) => {
            let target = as_map(
                source
                    .entry("body")
                    .or_insert_with(|| Value::Object(Map::new())),
            );
            target.insert("mode".into(), "urlencoded".into());
            let merged = merge_rows(target.get("urlencoded"), rows, false);
            target.insert("urlencoded".into(), merged);
        }
        Body::FormData(parts) => {
            let target = as_map(
                source
                    .entry("body")
                    .or_insert_with(|| Value::Object(Map::new())),
            );
            target.insert("mode".into(), "formdata".into());
            let old: Vec<Value> = target
                .get("formdata")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut used = vec![false; old.len()];
            let mut out = Vec::new();
            for part in parts {
                let found = old.iter().enumerate().find(|(index, value)| {
                    !used[*index]
                        && value.get("key").map(text).as_deref() == Some(part.key.as_str())
                });
                let mut object = match found {
                    Some((index, value)) => {
                        used[index] = true;
                        value.clone()
                    }
                    None => Value::Object(Map::new()),
                };
                let map = as_map(&mut object);
                map.insert("key".into(), part.key.clone().into());
                match part.kind {
                    PartKind::Text => {
                        map.insert("value".into(), part.value.clone().into());
                        map.insert("type".into(), "text".into());
                        map.remove("src");
                    }
                    PartKind::File => {
                        map.insert("src".into(), part.value.clone().into());
                        map.insert("type".into(), "file".into());
                        map.remove("value");
                    }
                }
                if part.content_type.is_empty() {
                    map.remove("contentType");
                } else {
                    map.insert("contentType".into(), part.content_type.clone().into());
                }
                if part.disabled {
                    map.insert("disabled".into(), true.into());
                } else {
                    map.remove("disabled");
                }
                out.push(object);
            }
            target.insert("formdata".into(), Value::Array(out));
        }
    }
}

/// Writes `auth` into `holder`, keeping parameters the file already had for
/// the same scheme (`showPassword`, ...).
fn write_auth(holder: &mut Map<String, Value>, auth: &Auth) {
    let (kind, params): (&str, Vec<(&str, String)>) = match auth {
        Auth::Inherit => {
            holder.remove("auth");
            return;
        }
        Auth::Unsupported(_) => return,
        Auth::None => ("noauth", Vec::new()),
        Auth::Basic { username, password } => (
            "basic",
            vec![
                ("username", username.clone()),
                ("password", password.clone()),
            ],
        ),
        Auth::Bearer { token } => ("bearer", vec![("token", token.clone())]),
        Auth::ApiKey {
            key,
            value,
            in_header,
        } => (
            "apikey",
            vec![
                ("key", key.clone()),
                ("value", value.clone()),
                ("in", if *in_header { "header" } else { "query" }.to_owned()),
            ],
        ),
    };
    let target = as_map(
        holder
            .entry("auth")
            .or_insert_with(|| Value::Object(Map::new())),
    );
    let same_kind = target.get("type").and_then(Value::as_str) == Some(kind);
    target.insert("type".into(), kind.into());
    if kind == "noauth" {
        return;
    }
    let mut rows: Vec<Value> = if same_kind {
        match target.get(kind) {
            Some(Value::Array(rows)) => rows.clone(),
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    for (name, value) in params {
        match rows
            .iter_mut()
            .find(|row| row.get("key").and_then(Value::as_str) == Some(name))
        {
            Some(row) => {
                as_map(row).insert("value".into(), value.into());
            }
            None => rows.push(json!({ "key": name, "value": value, "type": "string" })),
        }
    }
    target.insert(kind.into(), Value::Array(rows));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
	"info": {
		"_postman_id": "abc",
		"name": "Shop",
		"description": "The shop API",
		"schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
	},
	"auth": { "type": "bearer", "bearer": [ { "key": "token", "value": "{{token}}", "type": "string" } ] },
	"variable": [ { "key": "baseUrl", "value": "https://dummyjson.com" } ],
	"event": [ { "listen": "prerequest", "script": { "exec": [ "console.log(1)" ] } } ],
	"item": [
		{
			"name": "Products",
			"description": "folder note",
			"item": [
				{
					"name": "List Products",
					"protocolProfileBehavior": { "disableBodyPruning": true },
					"request": {
						"method": "GET",
						"header": [ { "key": "Accept", "value": "application/json", "description": "keep me" } ],
						"url": {
							"raw": "{{baseUrl}}/products/:id?limit=10&skip=0",
							"host": [ "{{baseUrl}}" ],
							"path": [ "products", ":id" ],
							"query": [
								{ "key": "limit", "value": "10", "description": "page size" },
								{ "key": "skip", "value": "0" },
								{ "key": "off", "value": "1", "disabled": true }
							],
							"variable": [ { "key": "id", "value": "5" } ]
						}
					},
					"response": [ { "name": "ok", "code": 200 } ]
				}
			]
		},
		{
			"name": "Login",
			"request": {
				"auth": { "type": "noauth" },
				"method": "POST",
				"header": [],
				"body": { "mode": "raw", "raw": "{\"a\":1}", "options": { "raw": { "language": "json" } } },
				"url": "https://x.test/login"
			}
		}
	]
}"#;

    #[test]
    fn the_tree_lists_folders_and_requests_in_order() {
        let c = Collection::parse(SAMPLE).unwrap();
        let rows: Vec<(usize, String, bool)> = c
            .entries()
            .into_iter()
            .map(|e| (e.depth, e.name, e.folder))
            .collect();
        assert_eq!(
            rows,
            vec![
                (0, "Products".to_owned(), true),
                (1, "List Products".to_owned(), false),
                (0, "Login".to_owned(), false)
            ]
        );
        assert_eq!(c.entries()[2].method.as_deref(), Some("POST"));
        assert_eq!(c.request_count(), 2);
    }

    #[test]
    fn a_request_reads_into_the_editable_shape() {
        let c = Collection::parse(SAMPLE).unwrap();
        let r = c.request(&[0, 0]).unwrap();
        assert_eq!(r.url, "{{baseUrl}}/products/:id");
        assert_eq!(r.query.len(), 3);
        assert!(r.query[2].disabled);
        assert_eq!(r.url_text(), "{{baseUrl}}/products/:id?limit=10&skip=0");
        assert_eq!(r.path_vars, vec![KeyValue::new("id", "5")]);
        assert_eq!(r.auth, Auth::Inherit);
        let login = c.request(&[1]).unwrap();
        assert_eq!(login.url, "https://x.test/login");
        assert_eq!(login.auth, Auth::None);
        assert_eq!(
            login.body,
            Body::Raw {
                text: "{\"a\":1}".into(),
                language: RawLanguage::Json
            }
        );
    }

    #[test]
    fn saving_without_edits_changes_nothing() {
        let once = Collection::parse(SAMPLE).unwrap().to_json();
        // Writing is stable: reading the output and writing it again is a no-op.
        assert_eq!(Collection::parse(&once).unwrap().to_json(), once);
        // Keys stay in the order the file had them, not alphabetical.
        let at = |needle: &str| once.find(needle).unwrap();
        assert!(at("\"_postman_id\"") < at("\"name\": \"Shop\""));
        assert!(at("\"info\"") < at("\"auth\""));
        assert!(at("\"auth\"") < at("\"variable\""));
        assert!(at("\"variable\"") < at("\"event\""));
        assert!(at("\"event\"") < at("\"item\""));
        assert!(at("\"method\": \"GET\"") < at("\"header\""));
    }

    #[test]
    fn writing_a_request_back_keeps_what_the_client_does_not_know() {
        let mut c = Collection::parse(SAMPLE).unwrap();
        let mut r = c.request(&[0, 0]).unwrap();
        r.query[0].value = "25".into();
        r.headers.push(KeyValue::new("X-Trace", "1"));
        r.description = "lists them".into();
        assert!(c.set_request(&[0, 0], &r));
        let json: Value = serde_json::from_str(&c.to_json()).unwrap();
        let item = &json["item"][0]["item"][0];
        assert_eq!(item["protocolProfileBehavior"]["disableBodyPruning"], true);
        assert_eq!(item["response"][0]["name"], "ok");
        assert_eq!(item["request"]["header"][0]["description"], "keep me");
        assert_eq!(item["request"]["header"][1]["key"], "X-Trace");
        assert_eq!(item["request"]["url"]["query"][0]["value"], "25");
        assert_eq!(
            item["request"]["url"]["query"][0]["description"],
            "page size"
        );
        assert_eq!(
            item["request"]["url"]["raw"],
            "{{baseUrl}}/products/:id?limit=25&skip=0"
        );
        assert_eq!(item["request"]["url"]["path"], json!(["products", ":id"]));
        assert_eq!(item["request"]["description"], "lists them");
        assert_eq!(json["event"][0]["listen"], "prerequest");
        assert_eq!(json["item"][0]["description"], "folder note");
        // And reading it again gives the same request.
        assert_eq!(c.request(&[0, 0]).unwrap(), r);
    }

    #[test]
    fn a_string_url_becomes_a_structured_one_on_write() {
        let mut c = Collection::parse(SAMPLE).unwrap();
        let mut r = c.request(&[1]).unwrap();
        r.set_url_text("https://x.test:8443/v1/login?next=/home");
        c.set_request(&[1], &r);
        let json: Value = serde_json::from_str(&c.to_json()).unwrap();
        let url = &json["item"][1]["request"]["url"];
        assert_eq!(url["protocol"], "https");
        assert_eq!(url["host"], json!(["x", "test"]));
        assert_eq!(url["port"], "8443");
        assert_eq!(url["path"], json!(["v1", "login"]));
        assert_eq!(url["query"][0]["key"], "next");
        assert_eq!(c.request(&[1]).unwrap().url_text(), r.url_text());
    }

    #[test]
    fn auth_is_inherited_from_the_nearest_parent() {
        let mut c = Collection::parse(SAMPLE).unwrap();
        assert_eq!(
            c.effective_auth(&[0, 0]),
            Auth::Bearer {
                token: "{{token}}".into()
            }
        );
        assert_eq!(c.effective_auth(&[1]), Auth::None);
        c.set_request(&[0, 0], &{
            let mut r = c.request(&[0, 0]).unwrap();
            r.auth = Auth::Basic {
                username: "u".into(),
                password: "p".into(),
            };
            r
        });
        assert_eq!(
            c.effective_auth(&[0, 0]),
            Auth::Basic {
                username: "u".into(),
                password: "p".into()
            }
        );
    }

    #[test]
    fn variables_stack_from_collection_to_folder() {
        let mut c = Collection::parse(SAMPLE).unwrap();
        c.node_mut(&[0]).unwrap().as_object_mut().unwrap().insert(
            "variable".into(),
            json!([{ "key": "baseUrl", "value": "http://local" }, { "key": "extra", "value": "1" }]),
        );
        let vars = c.variables(&[0, 0]);
        assert_eq!(
            vars,
            vec![
                KeyValue::new("baseUrl", "http://local"),
                KeyValue::new("extra", "1")
            ]
        );
        assert_eq!(
            c.variables(&[1]),
            vec![KeyValue::new("baseUrl", "https://dummyjson.com")]
        );
    }

    #[test]
    fn scripts_are_read_from_events_and_written_back_without_disturbing_others() {
        let mut c = Collection::parse(SAMPLE).unwrap();
        let above = c.scripts_above(&[0, 0], "prerequest");
        assert_eq!(
            above,
            vec![("Shop".to_owned(), "console.log(1)".to_owned())]
        );
        assert!(c.scripts_above(&[0, 0], "test").is_empty());
        let mut r = c.request(&[0, 0]).unwrap();
        assert!(r.pre_request.is_empty() && r.tests.is_empty());
        r.pre_request = "pm.variables.set('a', 1);\nconsole.log('x');".into();
        r.tests = "pm.test('ok', () => {});".into();
        assert!(c.set_request(&[0, 0], &r));
        let json: Value = serde_json::from_str(&c.to_json()).unwrap();
        let events = &json["item"][0]["item"][0]["event"];
        assert_eq!(events[0]["listen"], "prerequest");
        assert_eq!(
            events[0]["script"]["exec"],
            json!(["pm.variables.set('a', 1);", "console.log('x');"])
        );
        assert_eq!(events[0]["script"]["type"], "text/javascript");
        assert_eq!(events[1]["listen"], "test");
        assert_eq!(
            json["event"][0]["listen"], "prerequest",
            "the collection's own event stays"
        );
        assert_eq!(c.request(&[0, 0]).unwrap(), r);
        // Emptying a script removes its event, and then the empty list.
        r.pre_request.clear();
        r.tests.clear();
        c.set_request(&[0, 0], &r);
        let json: Value = serde_json::from_str(&c.to_json()).unwrap();
        assert!(json["item"][0]["item"][0].get("event").is_none());
        // A string exec and an unchanged script are left exactly as found.
        let text = r#"{"info":{"name":"x"},"item":[{"name":"a","event":[{"listen":"test","script":{"exec":"one line"}}],"request":{"method":"GET","url":"http://h"}}]}"#;
        let mut c = Collection::parse(text).unwrap();
        let r = c.request(&[0]).unwrap();
        assert_eq!(r.tests, "one line");
        let events_before = serde_json::to_string(&c.node(&[0]).unwrap()["event"]).unwrap();
        c.set_request(&[0], &r);
        let events_after = serde_json::to_string(&c.node(&[0]).unwrap()["event"]).unwrap();
        assert_eq!(events_after, events_before);
    }

    #[test]
    fn tree_edits_add_rename_duplicate_move_and_remove() {
        let mut c = Collection::new("New");
        let folder = c.add_folder(&[], "Users").unwrap();
        let req = c.add_request(&folder, "List").unwrap();
        assert_eq!(req, vec![0, 0]);
        assert!(
            c.add_request(&req, "bad").is_none(),
            "a request is not a folder"
        );
        let copy = c.duplicate(&req).unwrap();
        assert_eq!(c.item_name(&copy).as_deref(), Some("List copy"));
        c.rename(&req, "All");
        let top = c.add_request(&[], "Ping").unwrap();
        let moved = c.move_item(&top, &folder, 0).unwrap();
        assert_eq!(moved, vec![0, 0]);
        assert_eq!(c.item_name(&[0, 1]).as_deref(), Some("All"));
        assert!(c.move_item(&folder, &moved, 0).is_none(), "not into itself");
        assert!(c.remove(&[0, 0]));
        assert_eq!(c.request_count(), 2);
    }

    #[test]
    fn form_and_urlencoded_bodies_round_trip() {
        let mut c = Collection::new("B");
        let p = c.add_request(&[], "Upload").unwrap();
        let mut r = c.request(&p).unwrap();
        r.method = "post".into();
        r.body = Body::FormData(vec![
            FormPart {
                key: "a".into(),
                value: "1".into(),
                ..FormPart::default()
            },
            FormPart {
                key: "f".into(),
                value: "/tmp/x.png".into(),
                kind: PartKind::File,
                content_type: "image/png".into(),
                disabled: true,
            },
        ]);
        c.set_request(&p, &r);
        let back = c.request(&p).unwrap();
        assert_eq!(back.method, "POST");
        assert_eq!(back.body, r.body);
        r.body = Body::UrlEncoded(vec![KeyValue::new("k", "v")]);
        c.set_request(&p, &r);
        assert_eq!(c.request(&p).unwrap().body, r.body);
    }

    #[test]
    fn files_that_are_not_collections_are_refused_with_a_reason() {
        assert!(Collection::parse("[1]").is_err());
        assert!(Collection::parse("{\"name\":1}")
            .unwrap_err()
            .to_string()
            .contains("info"));
        let v1 = r#"{"info":{"name":"x","schema":"https://schema.getpostman.com/json/collection/v1.0.0/collection.json"}}"#;
        assert!(Collection::parse(v1)
            .unwrap_err()
            .to_string()
            .contains("v2.0 and v2.1"));
    }

    #[test]
    fn save_and_load_round_trip_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api").join("shop.postman_collection.json");
        let mut c = Collection::parse(SAMPLE).unwrap();
        c.set_path(path.clone());
        c.save().unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", c.to_json())
        );
        assert_eq!(Collection::load(&path).unwrap().name(), "Shop");
    }

    /// A collection of a few thousand requests loads, lists and saves in a
    /// fraction of a second. The bounds are loose; they catch a collapse in
    /// speed, not a slow disk.
    #[test]
    fn a_large_collection_is_quick_to_load_list_and_save() {
        use std::time::Instant;
        let mut items = Vec::new();
        for folder in 0..60 {
            let requests: Vec<Value> = (0..50)
                .map(|n| {
                    json!({
                        "name": format!("Request {folder}-{n}"),
                        "request": {
                            "method": "POST",
                            "header": [{"key": "Accept", "value": "application/json"}, {"key": "X-Id", "value": "{{id}}"}],
                            "body": {"mode": "raw", "raw": "{\"name\": \"value\", \"items\": [1, 2, 3, 4, 5]}", "options": {"raw": {"language": "json"}}},
                            "url": {"raw": "{{baseUrl}}/folder/:id/items?page=1&size=20", "host": ["{{baseUrl}}"], "path": ["folder", ":id", "items"],
                                "query": [{"key": "page", "value": "1"}, {"key": "size", "value": "20"}], "variable": [{"key": "id", "value": "7"}]},
                        },
                        "event": [{"listen": "test", "script": {"exec": ["pm.test('ok', () => {});"]}}],
                        "response": [{"name": "example", "code": 200, "body": "x".repeat(200)}],
                    })
                })
                .collect();
            items.push(json!({"name": format!("Folder {folder}"), "item": requests}));
        }
        let text = serde_json::to_string_pretty(&json!({
            "info": {"name": "Big", "schema": SCHEMA},
            "item": items,
        }))
        .unwrap();
        let started = Instant::now();
        let mut c = Collection::parse(&text).unwrap();
        let parse = started.elapsed();
        let started = Instant::now();
        let entries = c.entries();
        let list = started.elapsed();
        assert_eq!(entries.iter().filter(|e| !e.folder).count(), 3000);
        let started = Instant::now();
        let request = c.request(&[59, 49]).unwrap();
        assert!(c.set_request(&[59, 49], &request));
        let edit = started.elapsed();
        let started = Instant::now();
        let json = c.to_json();
        let write = started.elapsed();
        eprintln!(
            "3000 requests, {} KB: parse {parse:?}, list {list:?}, edit one {edit:?}, write {write:?}",
            json.len() / 1024
        );
        for (what, took) in [
            ("parse", parse),
            ("list", list),
            ("edit", edit),
            ("write", write),
        ] {
            assert!(took.as_secs_f64() < 5., "{what} took {took:?}");
        }
    }
}
