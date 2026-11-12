//! cURL in and out. Importing reads one pasted command (what browsers'
//! "Copy as cURL" and API docs produce) into an [`ApiRequest`]; exporting
//! writes a [`Prepared`] request as a command that can run in a shell.

use super::model::{ApiRequest, Auth, Body, FormPart, KeyValue, PartKind, RawLanguage};
use super::prepare::{PartContent, Prepared, PreparedBody};
use super::ApiError;

#[derive(Debug, Clone, PartialEq)]
pub struct CurlImport {
    pub request: ApiRequest,
    /// Options that were understood but have no effect here, or were not
    /// understood at all.
    pub warnings: Vec<String>,
    pub follow_redirects: bool,
}

struct Token {
    text: String,
    quoted: bool,
}

/// Splits a command line the way a POSIX shell does: single and double
/// quotes, backslashes, `$'...'` strings and `\` line continuations.
fn tokenize(input: &str) -> Result<Vec<Token>, ApiError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut i = 0;
    let unterminated = || ApiError::new("The command has an unclosed quote.");
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if chars.get(i + 1) == Some(&'\n') => i += 2,
            '\\' if chars.get(i + 1) == Some(&'\r') && chars.get(i + 2) == Some(&'\n') => i += 3,
            '\\' => {
                if let Some(next) = chars.get(i + 1) {
                    current.push(*next);
                    started = true;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '\'' => {
                started = true;
                quoted = true;
                i += 1;
                loop {
                    match chars.get(i) {
                        Some('\'') => break,
                        Some(ch) => current.push(*ch),
                        None => return Err(unterminated()),
                    }
                    i += 1;
                }
                i += 1;
            }
            '"' => {
                started = true;
                quoted = true;
                i += 1;
                loop {
                    match chars.get(i) {
                        Some('"') => break,
                        Some('\\') => match chars.get(i + 1) {
                            Some(next @ ('"' | '\\' | '$' | '`')) => {
                                current.push(*next);
                                i += 1;
                            }
                            Some('\n') => i += 1,
                            _ => current.push('\\'),
                        },
                        Some(ch) => current.push(*ch),
                        None => return Err(unterminated()),
                    }
                    i += 1;
                }
                i += 1;
            }
            '$' if chars.get(i + 1) == Some(&'\'') => {
                started = true;
                quoted = true;
                i += 2;
                loop {
                    match chars.get(i) {
                        Some('\'') => break,
                        Some('\\') => {
                            i += 1;
                            match chars.get(i) {
                                Some('n') => current.push('\n'),
                                Some('t') => current.push('\t'),
                                Some('r') => current.push('\r'),
                                Some('\\') => current.push('\\'),
                                Some('\'') => current.push('\''),
                                Some('"') => current.push('"'),
                                Some(kind @ ('x' | 'u' | 'U')) => {
                                    let width = match kind {
                                        'x' => 2,
                                        'u' => 4,
                                        _ => 8,
                                    };
                                    let digits: String = chars[i + 1..]
                                        .iter()
                                        .take(width)
                                        .take_while(|c| c.is_ascii_hexdigit())
                                        .collect();
                                    match u32::from_str_radix(&digits, 16)
                                        .ok()
                                        .and_then(char::from_u32)
                                    {
                                        Some(ch) if !digits.is_empty() => {
                                            current.push(ch);
                                            i += digits.len();
                                        }
                                        _ => {
                                            current.push('\\');
                                            current.push(*kind);
                                        }
                                    }
                                }
                                Some(other) => {
                                    current.push('\\');
                                    current.push(*other);
                                }
                                None => return Err(unterminated()),
                            }
                        }
                        Some(ch) => current.push(*ch),
                        None => return Err(unterminated()),
                    }
                    i += 1;
                }
                i += 1;
            }
            c if c.is_whitespace() => {
                if started {
                    tokens.push(Token {
                        text: std::mem::take(&mut current),
                        quoted,
                    });
                    started = false;
                    quoted = false;
                }
                i += 1;
            }
            other => {
                current.push(other);
                started = true;
                i += 1;
            }
        }
    }
    if started {
        tokens.push(Token {
            text: current,
            quoted,
        });
    }
    Ok(tokens)
}

/// Short options that take a value.
const SHORT_VALUE: &str = "XHdFubAeomTcCDKwxyYzrQ";
/// Long options that take a value.
const LONG_VALUE: &[&str] = &[
    "request",
    "header",
    "data",
    "data-raw",
    "data-binary",
    "data-ascii",
    "data-urlencode",
    "form",
    "form-string",
    "user",
    "cookie",
    "user-agent",
    "referer",
    "output",
    "max-time",
    "connect-timeout",
    "url",
    "json",
    "proxy",
    "cert",
    "key",
    "cacert",
    "retry",
    "range",
    "write-out",
    "oauth2-bearer",
    "resolve",
    "limit-rate",
    "max-redirs",
    "cookie-jar",
    "upload-file",
    "proxy-user",
    "aws-sigv4",
    "interface",
    "dns-servers",
    "capath",
    "pinnedpubkey",
    "retry-delay",
    "retry-max-time",
    "config",
    "ciphers",
    "unix-socket",
    "dump-header",
];

#[derive(Default)]
struct Parsed {
    method: Option<String>,
    url: Option<String>,
    headers: Vec<(String, String)>,
    data: Vec<String>,
    form: Vec<FormPart>,
    user: Option<String>,
    bearer: Option<String>,
    cookie: Option<String>,
    get: bool,
    head: bool,
    follow: bool,
    json: bool,
    warnings: Vec<String>,
}

impl Parsed {
    fn apply(&mut self, name: &str, value: Option<String>) {
        let value = value.unwrap_or_default();
        match name {
            "X" | "request" => self.method = Some(value.to_uppercase()),
            "H" | "header" => {
                // `Name;` (an empty header) and malformed ones are dropped.
                match value.split_once(':') {
                    Some((k, v)) if !k.trim().is_empty() => {
                        self.headers
                            .push((k.trim().to_owned(), v.trim().to_owned()));
                    }
                    _ => {}
                }
            }
            "d" | "data" | "data-ascii" | "data-binary" | "data-raw" | "json" => {
                if name == "json" {
                    self.json = true;
                }
                if name != "data-raw" && name != "json" && value.starts_with('@') {
                    self.warnings.push(format!(
                        "Reading the body from a file ({value}) is not imported; paste its content instead."
                    ));
                } else {
                    self.data.push(value);
                }
            }
            "data-urlencode" => {
                let encode = |s: &str| {
                    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>()
                };
                if let Some((key, content)) = value.split_once('=') {
                    self.data.push(if key.is_empty() {
                        encode(content)
                    } else {
                        format!("{key}={}", encode(content))
                    });
                } else if value.contains('@') {
                    self.warnings
                        .push(format!("Reading {value} from a file is not imported."));
                } else {
                    self.data.push(encode(&value));
                }
            }
            "F" | "form" | "form-string" => {
                let Some((key, content)) = value.split_once('=') else {
                    self.warnings
                        .push(format!("Skipped the form field {value:?}."));
                    return;
                };
                let (content, content_type) = match content.split_once(";type=") {
                    Some((content, kind)) => (content, kind.to_owned()),
                    None => (content, String::new()),
                };
                let file = name != "form-string" && content.starts_with('@');
                self.form.push(FormPart {
                    key: key.to_owned(),
                    value: if file {
                        content[1..].to_owned()
                    } else {
                        content.to_owned()
                    },
                    kind: if file { PartKind::File } else { PartKind::Text },
                    content_type,
                    disabled: false,
                });
            }
            "u" | "user" => self.user = Some(value),
            "oauth2-bearer" => self.bearer = Some(value),
            "b" | "cookie" => {
                if value.contains('=') {
                    self.cookie = Some(value);
                } else {
                    self.warnings.push("A cookie file is not imported.".into());
                }
            }
            "A" | "user-agent" => self.headers.push(("User-Agent".into(), value)),
            "e" | "referer" => self.headers.push(("Referer".into(), value)),
            "url" => self.url = Some(value),
            "G" | "get" => self.get = true,
            "I" | "head" => self.head = true,
            "L" | "location" | "location-trusted" => self.follow = true,
            "k" | "insecure" => self
                .warnings
                .push("-k/--insecure is ignored: certificates are always verified.".into()),
            // Output, transport and display options that do not change the request.
            "s"
            | "S"
            | "v"
            | "i"
            | "g"
            | "f"
            | "#"
            | "O"
            | "N"
            | "compressed"
            | "silent"
            | "show-error"
            | "verbose"
            | "include"
            | "globoff"
            | "fail"
            | "no-buffer"
            | "progress-bar"
            | "http1.1"
            | "http2"
            | "http1.0"
            | "http2-prior-knowledge"
            | "ipv4"
            | "ipv6"
            | "path-as-is"
            | "o"
            | "output"
            | "m"
            | "max-time"
            | "connect-timeout"
            | "max-redirs"
            | "retry"
            | "w"
            | "write-out"
            | "x"
            | "proxy"
            | "cacert"
            | "cert"
            | "key"
            | "tlsv1.2"
            | "tlsv1.3"
            | "no-keepalive"
            | "c"
            | "cookie-jar"
            | "D"
            | "dump-header"
            | "resolve"
            | "limit-rate" => {}
            other => self.warnings.push(format!("Ignored the option {other}.")),
        }
    }
}

pub fn parse_curl(command: &str) -> Result<CurlImport, ApiError> {
    let tokens = tokenize(command.trim())?;
    let mut words = tokens.into_iter().peekable();
    // A copied shell prompt.
    if words.peek().is_some_and(|t| t.text == "$" && !t.quoted) {
        words.next();
    }
    match words.next() {
        Some(token) if token.text == "curl" || token.text == "curl.exe" => {}
        _ => return Err(ApiError::new("A cURL command starts with the word curl.")),
    }
    let mut parsed = Parsed::default();
    while let Some(token) = words.next() {
        let text = token.text;
        if !token.quoted && matches!(text.as_str(), "|" | "&&" | ";" | ">" | ">>" | "||" | "2>&1") {
            break;
        }
        if token.quoted || !text.starts_with('-') || text == "-" {
            if parsed.url.is_none() {
                parsed.url = Some(text);
            } else {
                parsed
                    .warnings
                    .push("Only the first URL is imported.".into());
            }
            continue;
        }
        if let Some(long) = text.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
                None => (long.to_owned(), None),
            };
            let value = if LONG_VALUE.contains(&name.as_str()) {
                inline.or_else(|| words.next().map(|t| t.text))
            } else {
                None
            };
            parsed.apply(&name, value);
        } else {
            let flags: Vec<char> = text[1..].chars().collect();
            let mut index = 0;
            while index < flags.len() {
                let flag = flags[index];
                if SHORT_VALUE.contains(flag) {
                    let rest: String = flags[index + 1..].iter().collect();
                    let value = if rest.is_empty() {
                        words.next().map(|t| t.text)
                    } else {
                        Some(rest)
                    };
                    parsed.apply(&flag.to_string(), value);
                    break;
                }
                parsed.apply(&flag.to_string(), None);
                index += 1;
            }
        }
    }
    build(parsed)
}

fn build(mut parsed: Parsed) -> Result<CurlImport, ApiError> {
    let mut url = parsed
        .url
        .take()
        .ok_or_else(|| ApiError::new("The command has no URL."))?;
    if !url.contains("://") && !url.starts_with("{{") {
        url = format!("http://{url}");
    }
    let mut request = ApiRequest::default();
    let mut headers = std::mem::take(&mut parsed.headers);

    if parsed.json {
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        {
            headers.push(("Content-Type".into(), "application/json".into()));
        }
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept"))
        {
            headers.push(("Accept".into(), "application/json".into()));
        }
    }
    if let Some(cookie) = parsed.cookie.take() {
        headers.push(("Cookie".into(), cookie));
    }

    // Credentials: -u, --oauth2-bearer, or an Authorization header.
    request.auth = Auth::None;
    if let Some(user) = parsed.user.take() {
        let (username, password) = user.split_once(':').unwrap_or((&user, ""));
        request.auth = Auth::Basic {
            username: username.to_owned(),
            password: password.to_owned(),
        };
    } else if let Some(token) = parsed.bearer.take() {
        request.auth = Auth::Bearer { token };
    } else if let Some(position) = headers.iter().position(|(k, v)| {
        k.eq_ignore_ascii_case("authorization") && v.to_ascii_lowercase().starts_with("bearer ")
    }) {
        let (_, value) = headers.remove(position);
        request.auth = Auth::Bearer {
            token: value[7..].trim().to_owned(),
        };
    }

    let content_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.to_ascii_lowercase())
        .unwrap_or_default();

    let has_body = !parsed.data.is_empty() || !parsed.form.is_empty();
    if parsed.get && !parsed.data.is_empty() {
        // -G puts the data in the query string.
        let query = parsed.data.join("&");
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&query);
        parsed.data.clear();
    }
    request.set_url_text(&url);

    if !parsed.form.is_empty() {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
        request.body = Body::FormData(std::mem::take(&mut parsed.form));
    } else if !parsed.data.is_empty() {
        let text = parsed.data.join("&");
        let raw = |language| Body::Raw {
            text: text.clone(),
            language,
        };
        request.body = if content_type.contains("json") {
            raw(RawLanguage::Json)
        } else if content_type.contains("xml") {
            raw(RawLanguage::Xml)
        } else if content_type.contains("html") {
            raw(RawLanguage::Html)
        } else if content_type.contains("javascript") {
            raw(RawLanguage::JavaScript)
        } else if content_type.is_empty() || content_type.contains("x-www-form-urlencoded") {
            let pairs: Vec<KeyValue> = url::form_urlencoded::parse(text.as_bytes())
                .map(|(k, v)| KeyValue::new(k.into_owned(), v.into_owned()))
                .collect();
            if text.contains('=') && !pairs.is_empty() {
                headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
                Body::UrlEncoded(pairs)
            } else {
                if content_type.is_empty() {
                    // curl's default for -d, which a plain raw body would not send.
                    headers.push((
                        "Content-Type".into(),
                        "application/x-www-form-urlencoded".into(),
                    ));
                }
                raw(RawLanguage::Text)
            }
        } else {
            raw(RawLanguage::Text)
        };
    }

    request.method = match (&parsed.method, parsed.head, has_body && !parsed.get) {
        (Some(method), _, _) => method.clone(),
        (None, true, _) => "HEAD".into(),
        (None, false, true) => "POST".into(),
        _ => "GET".into(),
    };
    request.headers = headers
        .into_iter()
        .map(|(k, v)| KeyValue::new(k, v))
        .collect();
    request.name = default_name(&request);
    Ok(CurlImport {
        request,
        warnings: parsed.warnings,
        follow_redirects: parsed.follow,
    })
}

/// A name for an imported request: its method and last path segment, else
/// its host.
fn default_name(request: &ApiRequest) -> String {
    let after_scheme = request
        .url
        .split_once("://")
        .map_or(request.url.as_str(), |(_, rest)| rest);
    let (host, path) = after_scheme.split_once('/').unwrap_or((after_scheme, ""));
    let last = path.rsplit('/').find(|s| !s.is_empty());
    format!("{} {}", request.method, last.unwrap_or(host))
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The request as a shell command. Variables must already be filled in
/// (`Prepared` has them); anything still in `{{braces}}` is written as is.
pub fn to_curl(prepared: &Prepared) -> String {
    let mut lines: Vec<String> = vec![format!("curl {}", quote(&prepared.url))];
    let has_body = !matches!(prepared.body, PreparedBody::None);
    if prepared.method == "HEAD" {
        lines.push("--head".into());
    } else if prepared.method != "GET" && !(prepared.method == "POST" && has_body) {
        lines.push(format!("-X {}", prepared.method));
    }
    for (name, value) in &prepared.headers {
        lines.push(format!("-H {}", quote(&format!("{name}: {value}"))));
    }
    match &prepared.body {
        PreparedBody::None => {}
        PreparedBody::Bytes(bytes) => {
            lines.push(format!(
                "--data-raw {}",
                quote(&String::from_utf8_lossy(bytes))
            ));
        }
        PreparedBody::Multipart(parts) => {
            for part in parts {
                let mut field = match &part.content {
                    PartContent::Text(text) => format!("{}={text}", part.name),
                    PartContent::File(path) => format!("{}=@{}", part.name, path.display()),
                };
                if let Some(content_type) = &part.content_type {
                    field.push_str(&format!(";type={content_type}"));
                }
                lines.push(format!("-F {}", quote(&field)));
            }
        }
    }
    lines.join(" \\\n  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::prepare::{prepare, scope_of};

    fn parse(command: &str) -> CurlImport {
        parse_curl(command).unwrap()
    }

    #[test]
    fn a_plain_get_needs_only_a_url() {
        let c = parse("curl https://dummyjson.com/products?limit=10");
        assert_eq!(c.request.method, "GET");
        assert_eq!(c.request.url, "https://dummyjson.com/products");
        assert_eq!(c.request.query, vec![KeyValue::new("limit", "10")]);
        assert_eq!(c.request.name, "GET products");
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn a_json_post_from_the_browser_keeps_its_headers_and_body() {
        let c = parse(
            "curl 'https://x.test/auth/login' \\\n  -H 'Content-Type: application/json' \\\n  -H 'accept: */*' \\\n  --data-raw '{\"username\":\"emilys\",\"password\":\"it'\\''s\"}' \\\n  --compressed",
        );
        assert_eq!(c.request.method, "POST");
        assert_eq!(
            c.request.body,
            Body::Raw {
                text: "{\"username\":\"emilys\",\"password\":\"it's\"}".into(),
                language: RawLanguage::Json
            }
        );
        assert_eq!(c.request.headers.len(), 2);
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn dollar_quoted_strings_decode_their_escapes() {
        let c = parse(
            "curl -X POST https://x.test -H 'Content-Type: application/json' --data-raw $'{\"a\":\"line\\nbreak \\u00e9\"}'",
        );
        match c.request.body {
            Body::Raw { text, .. } => assert_eq!(text, "{\"a\":\"line\nbreak é\"}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn form_data_becomes_a_urlencoded_table_and_uploads_become_parts() {
        let c = parse("curl https://x.test -d 'a=1&b=two%20words' -d c=3");
        assert_eq!(c.request.method, "POST");
        assert_eq!(
            c.request.body,
            Body::UrlEncoded(vec![
                KeyValue::new("a", "1"),
                KeyValue::new("b", "two words"),
                KeyValue::new("c", "3"),
            ])
        );
        assert!(c.request.headers.is_empty());
        let c = parse("curl https://x.test -F name=Ann -F 'photo=@/tmp/a.png;type=image/png'");
        match c.request.body {
            Body::FormData(parts) => {
                assert_eq!(parts[0].value, "Ann");
                assert_eq!(parts[1].kind, PartKind::File);
                assert_eq!(parts[1].value, "/tmp/a.png");
                assert_eq!(parts[1].content_type, "image/png");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn credentials_become_the_auth_tab() {
        let c = parse("curl -u me:secret https://x.test");
        assert_eq!(
            c.request.auth,
            Auth::Basic {
                username: "me".into(),
                password: "secret".into()
            }
        );
        let c = parse("curl https://x.test -H 'Authorization: Bearer abc.def'");
        assert_eq!(
            c.request.auth,
            Auth::Bearer {
                token: "abc.def".into()
            }
        );
        assert!(c.request.headers.is_empty());
    }

    #[test]
    fn method_flags_and_get_mode_are_understood() {
        assert_eq!(parse("curl -I https://x.test").request.method, "HEAD");
        assert_eq!(
            parse("curl -XDELETE https://x.test/1").request.method,
            "DELETE"
        );
        let c = parse("curl -G https://x.test/s -d q=rust --data-urlencode 'n=a b'");
        assert_eq!(c.request.method, "GET");
        assert_eq!(c.request.url_text(), "https://x.test/s?q=rust&n=a+b");
        assert_eq!(c.request.body, Body::None);
    }

    #[test]
    fn combined_short_flags_and_redirects_are_read() {
        let c = parse("curl -sSL https://x.test");
        assert!(c.follow_redirects);
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn insecure_unknown_and_file_inputs_are_called_out() {
        let c = parse("curl -k --frobnicate -d @body.json https://x.test");
        assert_eq!(c.warnings.len(), 3);
        assert!(c.warnings.iter().any(|w| w.contains("insecure")));
        assert!(c.warnings.iter().any(|w| w.contains("frobnicate")));
        assert!(c.warnings.iter().any(|w| w.contains("body.json")));
    }

    #[test]
    fn anything_after_a_pipe_is_not_part_of_the_request() {
        let c = parse("$ curl https://x.test/a | jq .");
        assert_eq!(c.request.url, "https://x.test/a");
    }

    #[test]
    fn bad_input_gets_a_reason() {
        assert!(parse_curl("wget https://x")
            .unwrap_err()
            .to_string()
            .contains("curl"));
        assert!(parse_curl("curl 'https://x")
            .unwrap_err()
            .to_string()
            .contains("quote"));
        assert!(parse_curl("curl -X POST")
            .unwrap_err()
            .to_string()
            .contains("URL"));
        assert_eq!(
            parse("curl example.com/x").request.url,
            "http://example.com/x"
        );
    }

    #[test]
    fn export_then_import_gives_the_same_request() {
        let mut request = ApiRequest {
            method: "POST".into(),
            ..ApiRequest::default()
        };
        request.set_url_text("{{base}}/items?x=1");
        request.headers.push(KeyValue::new("X-A", "it's"));
        request.body = Body::Raw {
            text: "{\"k\": \"v's\"}".into(),
            language: RawLanguage::Json,
        };
        let prepared = prepare(
            &request,
            &Auth::Bearer {
                token: "tok".into(),
            },
            &scope_of(&[("base", "https://h.test")]),
        );
        let command = to_curl(&prepared);
        assert!(command.starts_with("curl 'https://h.test/items?x=1' \\\n  -H 'X-A: it'\\''s'"));
        assert!(!command.contains("-X"), "a POST with a body needs no -X");
        let back = parse(&command).request;
        assert_eq!(back.method, "POST");
        assert_eq!(back.url_text(), "https://h.test/items?x=1");
        assert_eq!(back.body, request.body);
        assert_eq!(
            back.auth,
            Auth::Bearer {
                token: "tok".into()
            }
        );
        assert!(back
            .headers
            .iter()
            .any(|h| h.key == "X-A" && h.value == "it's"));
    }

    #[test]
    fn exporting_other_methods_and_multipart() {
        let mut request = ApiRequest {
            method: "DELETE".into(),
            ..ApiRequest::default()
        };
        request.set_url_text("https://h.test/1");
        let none = crate::api::Scope::new();
        let command = to_curl(&prepare(&request, &Auth::None, &none));
        assert!(command.contains("-X DELETE"));
        request.method = "POST".into();
        request.body = Body::FormData(vec![
            FormPart {
                key: "a".into(),
                value: "1".into(),
                ..FormPart::default()
            },
            FormPart {
                key: "f".into(),
                value: "/tmp/x".into(),
                kind: PartKind::File,
                ..FormPart::default()
            },
        ]);
        let command = to_curl(&prepare(&request, &Auth::None, &none));
        assert!(command.contains("-F 'a=1'") && command.contains("-F 'f=@/tmp/x'"));
    }
}
