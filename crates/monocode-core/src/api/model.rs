//! The editable shape of one API request. Postman's JSON stays the source of
//! truth ([`super::collection`]); these types are what the UI edits, and
//! writing them back merges into the original item so unknown fields survive.

use serde::{Deserialize, Serialize};

/// One row of a key/value table (query, header, form field, variable).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
    pub disabled: bool,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            disabled: false,
        }
    }
}

/// The syntax a raw body is highlighted and labelled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RawLanguage {
    #[default]
    Text,
    Json,
    Xml,
    Html,
    JavaScript,
}

impl RawLanguage {
    /// Postman's `options.raw.language` value.
    pub fn id(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Xml => "xml",
            Self::Html => "html",
            Self::JavaScript => "javascript",
        }
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "json" => Self::Json,
            "xml" => Self::Xml,
            "html" => Self::Html,
            "javascript" => Self::JavaScript,
            _ => Self::Text,
        }
    }

    /// The `Content-Type` sent when the request sets none of its own.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Text => "text/plain",
            Self::Json => "application/json",
            Self::Xml => "application/xml",
            Self::Html => "text/html",
            Self::JavaScript => "application/javascript",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PartKind {
    #[default]
    Text,
    File,
}

/// One `multipart/form-data` field. A file part names a path in `value`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FormPart {
    pub key: String,
    pub value: String,
    pub kind: PartKind,
    pub content_type: String,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Body {
    #[default]
    None,
    Raw {
        text: String,
        language: RawLanguage,
    },
    UrlEncoded(Vec<KeyValue>),
    FormData(Vec<FormPart>),
    /// A mode this client cannot edit (`graphql`, `file`, ...). The body is
    /// left untouched in the file and not sent.
    Unsupported(String),
}

impl Body {
    pub fn label(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Raw { .. } => "raw",
            Self::UrlEncoded(_) => "x-www-form-urlencoded",
            Self::FormData(_) => "form-data",
            Self::Unsupported(mode) => mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Auth {
    /// No auth of its own: the folder's or collection's applies.
    #[default]
    Inherit,
    None,
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
    },
    ApiKey {
        key: String,
        value: String,
        /// `true` sends it as a header, `false` as a query parameter.
        in_header: bool,
    },
    /// A scheme that is kept in the file but not executed (OAuth, AWS, ...).
    Unsupported(String),
}

impl Auth {
    pub fn label(&self) -> &str {
        match self {
            Self::Inherit => "Inherit from parent",
            Self::None => "No auth",
            Self::Basic { .. } => "Basic",
            Self::Bearer { .. } => "Bearer token",
            Self::ApiKey { .. } => "API key",
            Self::Unsupported(kind) => kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiRequest {
    pub name: String,
    pub method: String,
    /// The address without its query string (may hold `{{variables}}`).
    pub url: String,
    pub query: Vec<KeyValue>,
    /// Values for `:name` segments of the path.
    pub path_vars: Vec<KeyValue>,
    pub headers: Vec<KeyValue>,
    pub body: Body,
    pub auth: Auth,
    pub description: String,
    /// The request's own pre-request script (collection and folder scripts
    /// are read from their own items).
    pub pre_request: String,
    /// The request's own test script.
    pub tests: String,
}

impl Default for ApiRequest {
    fn default() -> Self {
        Self {
            name: String::new(),
            method: "GET".into(),
            url: String::new(),
            query: Vec::new(),
            path_vars: Vec::new(),
            headers: Vec::new(),
            body: Body::None,
            auth: Auth::Inherit,
            description: String::new(),
            pre_request: String::new(),
            tests: String::new(),
        }
    }
}

impl ApiRequest {
    /// The address as the URL bar shows it: the base plus the enabled query.
    pub fn url_text(&self) -> String {
        let mut text = self.url.clone();
        let mut first = true;
        for param in self.query.iter().filter(|p| !p.disabled) {
            text.push(if first { '?' } else { '&' });
            first = false;
            text.push_str(&param.key);
            if !param.value.is_empty() || param.key.is_empty() {
                text.push('=');
                text.push_str(&param.value);
            }
        }
        text
    }

    /// Takes what was typed in the URL bar: the part before `?` becomes the
    /// base and the enabled query rows are replaced by the typed ones, in
    /// order. Disabled rows stay where they are, after the typed ones.
    pub fn set_url_text(&mut self, text: &str) {
        let (base, query) = match text.split_once('?') {
            Some((base, query)) => (base, query),
            None => (text, ""),
        };
        self.url = base.to_owned();
        let disabled: Vec<KeyValue> = self.query.iter().filter(|p| p.disabled).cloned().collect();
        let mut rows: Vec<KeyValue> = Vec::new();
        if !query.is_empty() {
            for pair in query.split('&') {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                rows.push(KeyValue::new(key, value));
            }
        }
        // Keep the descriptions-free rows stable: typed rows first.
        rows.extend(disabled);
        self.query = rows;
        self.sync_path_vars();
    }

    /// The `:name` segments of the URL's path, in order.
    pub fn path_var_names(&self) -> Vec<String> {
        path_var_names(&self.url)
    }

    /// Keeps `path_vars` in step with the URL: one row per `:name`, existing
    /// values kept.
    pub fn sync_path_vars(&mut self) {
        let names = self.path_var_names();
        let old = std::mem::take(&mut self.path_vars);
        self.path_vars = names
            .into_iter()
            .map(|name| {
                old.iter()
                    .find(|row| row.key == name)
                    .cloned()
                    .unwrap_or_else(|| KeyValue::new(name, ""))
            })
            .collect();
    }
}

/// `:name` segments of a URL's path. The scheme's colon, a port and
/// `{{variables}}` are not path variables.
pub fn path_var_names(url: &str) -> Vec<String> {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let Some(slash) = after_scheme.find('/') else {
        return Vec::new();
    };
    let path = &after_scheme[slash..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut names: Vec<String> = Vec::new();
    for segment in path.split('/') {
        match segment.strip_prefix(':') {
            Some(name) if !name.is_empty() && !names.iter().any(|known| known == name) => {
                names.push(name.to_owned());
            }
            _ => {}
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_text_round_trips_through_the_query_table() {
        let mut request = ApiRequest::default();
        request.set_url_text("{{baseUrl}}/products?limit=10&skip=0&select=title,price");
        assert_eq!(request.url, "{{baseUrl}}/products");
        assert_eq!(request.query.len(), 3);
        assert_eq!(request.query[2], KeyValue::new("select", "title,price"));
        assert_eq!(
            request.url_text(),
            "{{baseUrl}}/products?limit=10&skip=0&select=title,price"
        );
    }

    #[test]
    fn disabled_rows_stay_out_of_the_bar_and_survive_edits() {
        let mut request = ApiRequest::default();
        request.set_url_text("http://a/b?x=1&y=2");
        request.query[0].disabled = true;
        assert_eq!(request.url_text(), "http://a/b?y=2");
        request.set_url_text("http://a/b?y=3");
        assert_eq!(request.url_text(), "http://a/b?y=3");
        assert!(request.query.iter().any(|p| p.disabled && p.key == "x"));
    }

    #[test]
    fn a_valueless_parameter_has_no_equals_sign() {
        let mut request = ApiRequest::default();
        request.set_url_text("http://a/b?flag&x=");
        assert_eq!(request.query[0], KeyValue::new("flag", ""));
        assert_eq!(request.url_text(), "http://a/b?flag&x");
    }

    #[test]
    fn path_variables_ignore_scheme_port_and_templates() {
        assert_eq!(
            path_var_names("http://localhost:8080/users/:id/posts/:post?x=:no"),
            vec!["id", "post"]
        );
        assert!(path_var_names("{{baseUrl}}").is_empty());
        let mut request = ApiRequest::default();
        request.set_url_text("{{baseUrl}}/users/:id");
        request.path_vars[0].value = "7".into();
        request.set_url_text("{{baseUrl}}/users/:id/x/:other");
        assert_eq!(request.path_vars[0], KeyValue::new("id", "7"));
        assert_eq!(request.path_vars[1], KeyValue::new("other", ""));
    }
}
