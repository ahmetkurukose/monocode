//! Turns an edited request into what goes on the wire: variables
//! substituted, enabled rows only, auth applied, body encoded.

use super::model::{path_var_names, ApiRequest, Auth, Body, KeyValue, PartKind};
use super::vars::Scope;
use base64::Engine as _;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartContent {
    Text(String),
    File(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartPart {
    pub name: String,
    pub content: PartContent,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PreparedBody {
    #[default]
    None,
    Bytes(Vec<u8>),
    Multipart(Vec<MultipartPart>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: PreparedBody,
    /// Variables that had no value; they went out as written.
    pub missing: Vec<String>,
    /// Variables that are defined but empty; they went out as nothing.
    pub empty: Vec<String>,
    /// Things the request asked for that this client does not do.
    pub notes: Vec<String>,
}

impl Prepared {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn encode_path_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '/' => out.push_str("%2F"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            other => out.push(other),
        }
    }
    out
}

/// Replaces `:name` path segments with their values.
fn fill_path_vars(url: &str, values: &[(String, String)]) -> String {
    if path_var_names(url).is_empty() {
        return url.to_owned();
    }
    let split_at = url.find("://").map_or(0, |i| i + 3);
    let Some(slash) = url[split_at..].find('/').map(|i| i + split_at) else {
        return url.to_owned();
    };
    let (head, tail) = url.split_at(slash);
    let (path, rest) = match tail.find(['?', '#']) {
        Some(i) => tail.split_at(i),
        None => (tail, ""),
    };
    let path = path
        .split('/')
        .map(|segment| match segment.strip_prefix(':') {
            Some(name) => values
                .iter()
                .find(|(key, value)| key == name && !value.is_empty())
                .map(|(_, value)| encode_path_value(value))
                .unwrap_or_else(|| segment.to_owned()),
            None => segment.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("/");
    format!("{head}{path}{rest}")
}

/// `auth` is the one that applies ([`super::Collection::effective_auth`]).
pub fn prepare(request: &ApiRequest, auth: &Auth, scope: &Scope) -> Prepared {
    let mut missing: Vec<String> = Vec::new();
    let mut empty: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut resolve = |text: &str| -> String {
        let resolved = scope.resolve(text);
        for name in resolved.missing {
            if !missing.contains(&name) {
                missing.push(name);
            }
        }
        for name in resolved.empty {
            if !empty.contains(&name) {
                empty.push(name);
            }
        }
        resolved.text
    };

    let path_values: Vec<(String, String)> = request
        .path_vars
        .iter()
        .map(|row| (row.key.clone(), resolve(&row.value)))
        .collect();
    let mut url = fill_path_vars(&resolve(&request.url), &path_values);
    let mut query: Vec<(String, String)> = request
        .query
        .iter()
        .filter(|row| !row.disabled && !(row.key.is_empty() && row.value.is_empty()))
        .map(|row| (resolve(&row.key), resolve(&row.value)))
        .collect();

    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .filter(|row| !row.disabled && !row.key.trim().is_empty())
        .map(|row| (resolve(row.key.trim()), resolve(&row.value)))
        .collect();
    let has = |headers: &[(String, String)], name: &str| {
        headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case(name))
    };

    match auth {
        Auth::Basic { username, password } => {
            if !has(&headers, "authorization") {
                let pair = format!("{}:{}", resolve(username), resolve(password));
                let token = base64::engine::general_purpose::STANDARD.encode(pair);
                headers.push(("Authorization".into(), format!("Basic {token}")));
            }
        }
        Auth::Bearer { token } => {
            if !has(&headers, "authorization") {
                headers.push(("Authorization".into(), format!("Bearer {}", resolve(token))));
            }
        }
        Auth::ApiKey {
            key,
            value,
            in_header,
        } => {
            let (key, value) = (resolve(key), resolve(value));
            if !key.is_empty() {
                if *in_header {
                    if !has(&headers, &key) {
                        headers.push((key, value));
                    }
                } else {
                    query.push((key, value));
                }
            }
        }
        Auth::Unsupported(kind) => {
            notes.push(format!(
                "{kind} auth is kept in the file but not applied when sending."
            ));
        }
        Auth::Inherit | Auth::None => {}
    }

    if !query.is_empty() {
        // Anything after `#` is a fragment and is never sent.
        let (base, fragment) = match url.split_once('#') {
            Some((base, fragment)) => (base.to_owned(), Some(fragment.to_owned())),
            None => (url.clone(), None),
        };
        let mut joined = base;
        let mut sep = if joined.contains('?') { '&' } else { '?' };
        for (key, value) in &query {
            joined.push(sep);
            sep = '&';
            joined.push_str(key);
            if !value.is_empty() {
                joined.push('=');
                joined.push_str(value);
            }
        }
        if let Some(fragment) = fragment {
            joined.push('#');
            joined.push_str(&fragment);
        }
        url = joined;
    }

    let mut body = PreparedBody::None;
    match &request.body {
        Body::None => {}
        Body::Raw { text, language } => {
            if !has(&headers, "content-type") {
                headers.push(("Content-Type".into(), language.content_type().into()));
            }
            body = PreparedBody::Bytes(resolve(text).into_bytes());
        }
        Body::UrlEncoded(rows) => {
            if !has(&headers, "content-type") {
                headers.push((
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ));
            }
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for row in rows.iter().filter(|r| !r.disabled && !r.key.is_empty()) {
                serializer.append_pair(&resolve(&row.key), &resolve(&row.value));
            }
            body = PreparedBody::Bytes(serializer.finish().into_bytes());
        }
        Body::FormData(parts) => {
            // The boundary goes into Content-Type when the body is built.
            headers.retain(|(key, _)| !key.eq_ignore_ascii_case("content-type"));
            body = PreparedBody::Multipart(
                parts
                    .iter()
                    .filter(|p| !p.disabled && !p.key.is_empty())
                    .map(|part| MultipartPart {
                        name: resolve(&part.key),
                        content: match part.kind {
                            PartKind::Text => PartContent::Text(resolve(&part.value)),
                            PartKind::File => {
                                PartContent::File(PathBuf::from(resolve(&part.value)))
                            }
                        },
                        content_type: (!part.content_type.is_empty())
                            .then(|| resolve(&part.content_type)),
                    })
                    .collect(),
            );
        }
        Body::Unsupported(mode) => {
            notes.push(format!("A {mode} body is kept in the file but not sent."));
        }
    }

    if !has(&headers, "user-agent") {
        headers.push(("User-Agent".into(), "Monocode".into()));
    }
    if !has(&headers, "accept") {
        headers.push(("Accept".into(), "*/*".into()));
    }

    Prepared {
        method: request.method.trim().to_uppercase(),
        url: url.trim().to_owned(),
        headers,
        body,
        missing,
        empty,
        notes,
    }
}

/// Used by tests and the cURL exporter to build a scope from plain rows.
#[allow(dead_code)]
pub(crate) fn scope_of(rows: &[(&str, &str)]) -> Scope {
    let mut scope = Scope::new();
    scope.push(rows.iter().map(|(k, v)| KeyValue::new(*k, *v)));
    scope
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{FormPart, RawLanguage};

    fn request(url: &str) -> ApiRequest {
        let mut r = ApiRequest::default();
        r.set_url_text(url);
        r
    }

    #[test]
    fn variables_path_values_and_query_are_filled() {
        let mut r = request("{{base}}/users/:id?limit=10&off=1");
        r.query[1].disabled = true;
        r.path_vars[0].value = "{{uid}}".into();
        let p = prepare(
            &r,
            &Auth::None,
            &scope_of(&[("base", "https://x.test"), ("uid", "a b")]),
        );
        assert_eq!(p.url, "https://x.test/users/a%20b?limit=10");
        assert!(p.missing.is_empty());
    }

    #[test]
    fn empty_variables_are_reported_apart_from_missing_ones() {
        let r = request("{{url}}/flow/history");
        let p = prepare(&r, &Auth::None, &scope_of(&[("url", "")]));
        assert_eq!(p.url, "/flow/history");
        assert!(p.missing.is_empty());
        assert_eq!(p.empty, vec!["url"]);
    }

    #[test]
    fn missing_variables_are_reported_and_left_in_place() {
        let r = request("{{base}}/x?k={{nope}}");
        let p = prepare(&r, &Auth::None, &Scope::new());
        assert_eq!(p.url, "{{base}}/x?k={{nope}}");
        assert_eq!(p.missing, vec!["base", "nope"]);
    }

    #[test]
    fn auth_becomes_a_header_or_a_query_parameter() {
        let r = request("http://a/b?x=1");
        let basic = prepare(
            &r,
            &Auth::Basic {
                username: "u".into(),
                password: "p".into(),
            },
            &Scope::new(),
        );
        assert_eq!(basic.header("authorization"), Some("Basic dTpw"));
        let bearer = prepare(
            &r,
            &Auth::Bearer {
                token: "{{t}}".into(),
            },
            &scope_of(&[("t", "abc")]),
        );
        assert_eq!(bearer.header("authorization"), Some("Bearer abc"));
        let key = prepare(
            &r,
            &Auth::ApiKey {
                key: "api_key".into(),
                value: "k".into(),
                in_header: false,
            },
            &Scope::new(),
        );
        assert_eq!(key.url, "http://a/b?x=1&api_key=k");
        let header_key = prepare(
            &r,
            &Auth::ApiKey {
                key: "X-Key".into(),
                value: "k".into(),
                in_header: true,
            },
            &Scope::new(),
        );
        assert_eq!(header_key.header("x-key"), Some("k"));
    }

    #[test]
    fn an_explicit_authorization_header_beats_the_auth_tab() {
        let mut r = request("http://a/b");
        r.headers.push(KeyValue::new("authorization", "Custom z"));
        let p = prepare(&r, &Auth::Bearer { token: "t".into() }, &Scope::new());
        assert_eq!(p.header("authorization"), Some("Custom z"));
        assert_eq!(
            p.headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                .count(),
            1
        );
    }

    #[test]
    fn bodies_are_encoded_with_their_content_type() {
        let mut r = request("http://a/b");
        r.body = Body::Raw {
            text: "{\"a\":\"{{v}}\"}".into(),
            language: RawLanguage::Json,
        };
        let p = prepare(&r, &Auth::None, &scope_of(&[("v", "1")]));
        assert_eq!(p.body, PreparedBody::Bytes(b"{\"a\":\"1\"}".to_vec()));
        assert_eq!(p.header("content-type"), Some("application/json"));

        r.body = Body::UrlEncoded(vec![KeyValue::new("q", "a b&c"), KeyValue::new("z", "1")]);
        let p = prepare(&r, &Auth::None, &Scope::new());
        assert_eq!(p.body, PreparedBody::Bytes(b"q=a+b%26c&z=1".to_vec()));
        assert_eq!(
            p.header("content-type"),
            Some("application/x-www-form-urlencoded")
        );

        r.headers.push(KeyValue::new("Content-Type", "text/csv"));
        r.body = Body::FormData(vec![FormPart {
            key: "a".into(),
            value: "1".into(),
            ..FormPart::default()
        }]);
        let p = prepare(&r, &Auth::None, &Scope::new());
        assert!(matches!(p.body, PreparedBody::Multipart(ref parts) if parts.len() == 1));
        assert_eq!(p.header("content-type"), None);
    }

    #[test]
    fn unsupported_things_are_noted_not_hidden() {
        let mut r = request("http://a/b");
        r.body = Body::Unsupported("graphql".into());
        let p = prepare(&r, &Auth::Unsupported("oauth2".into()), &Scope::new());
        assert_eq!(p.notes.len(), 2);
        assert_eq!(p.body, PreparedBody::None);
    }

    #[test]
    fn the_fragment_is_kept_after_the_query() {
        let r = request("http://a/b#top");
        let p = prepare(
            &r,
            &Auth::ApiKey {
                key: "k".into(),
                value: "v".into(),
                in_header: false,
            },
            &Scope::new(),
        );
        assert_eq!(p.url, "http://a/b?k=v#top");
    }
}
