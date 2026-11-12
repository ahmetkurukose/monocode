//! Sends a [`Prepared`] request. Blocking: run it off the UI thread. A
//! request in flight cannot be interrupted, only abandoned (the timeout
//! bounds it), so callers drop the result of a request the user cancelled.

use super::prepare::{MultipartPart, PartContent, Prepared, PreparedBody};
use super::ApiError;
use std::io::Read;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct SendOptions {
    pub timeout: Duration,
    /// How many redirects to follow; 0 follows none.
    pub max_redirects: u32,
    /// Bytes of the body kept; the rest is counted but dropped.
    pub max_body: usize,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            max_redirects: 10,
            max_body: 16 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The body was longer than [`SendOptions::max_body`].
    pub truncated: bool,
    pub elapsed: Duration,
    /// Where the response came from, after redirects.
    pub url: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn content_type(&self) -> &str {
        self.header("content-type").unwrap_or("")
    }

    pub fn is_json(&self) -> bool {
        let ct = self.content_type().to_ascii_lowercase();
        ct.contains("json") || serde_json::from_slice::<serde_json::Value>(&self.body).is_ok()
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// JSON indented for reading, or `None` when the body is not JSON.
    pub fn pretty_json(&self) -> Option<String> {
        let value: serde_json::Value = serde_json::from_slice(&self.body).ok()?;
        serde_json::to_string_pretty(&value).ok()
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The body of a multipart request and the `Content-Type` that names its
/// boundary.
pub fn multipart_body(parts: &[MultipartPart]) -> Result<(Vec<u8>, String), ApiError> {
    let boundary = format!("----MonocodeBoundary{}", uuid::Uuid::new_v4().simple());
    let mut out: Vec<u8> = Vec::new();
    for part in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        match &part.content {
            PartContent::Text(text) => {
                out.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"\r\n",
                        quote(&part.name)
                    )
                    .as_bytes(),
                );
                if let Some(content_type) = &part.content_type {
                    out.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
                }
                out.extend_from_slice(b"\r\n");
                out.extend_from_slice(text.as_bytes());
            }
            PartContent::File(path) => {
                let data = std::fs::read(path).map_err(|e| {
                    ApiError::new(format!("Could not read the file {}: {e}", path.display()))
                })?;
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                        quote(&part.name),
                        quote(&file_name)
                    )
                    .as_bytes(),
                );
                let content_type = part
                    .content_type
                    .as_deref()
                    .unwrap_or("application/octet-stream");
                out.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
                out.extend_from_slice(&data);
            }
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok((out, format!("multipart/form-data; boundary={boundary}")))
}

pub fn send(prepared: &Prepared, options: &SendOptions) -> Result<Response, ApiError> {
    let url = prepared.url.trim();
    if url.is_empty() {
        return Err(ApiError::new("Enter a URL first."));
    }
    let names = |list: &[String]| {
        list.iter()
            .map(|name| format!("{{{{{name}}}}}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !prepared.missing.is_empty() {
        return Err(ApiError::new(format!(
            "No value for {}. Pick an environment or define it as a variable.",
            names(&prepared.missing)
        )));
    }
    if !url.contains("://") {
        return Err(ApiError::new(if !prepared.empty.is_empty() {
            format!(
                "{} is empty, so the URL has no scheme or host. Give it a value in an \
                 environment or in the collection's variables.",
                names(&prepared.empty)
            )
        } else if url.starts_with('/') {
            format!("The URL has no host. Start it with a scheme and host, like https://example.com{url}")
        } else {
            format!("The URL needs a scheme, like https://{url}")
        }));
    }

    let agent = ureq::AgentBuilder::new()
        .timeout(options.timeout)
        .redirects(options.max_redirects)
        .build();
    let mut request = agent.request(&prepared.method, url);
    let mut multipart_type = None;
    let payload: Vec<u8> = match &prepared.body {
        PreparedBody::None => Vec::new(),
        PreparedBody::Bytes(bytes) => bytes.clone(),
        PreparedBody::Multipart(parts) => {
            let (bytes, content_type) = multipart_body(parts)?;
            multipart_type = Some(content_type);
            bytes
        }
    };
    for (name, value) in &prepared.headers {
        request = request.set(name, value);
    }
    if let Some(content_type) = &multipart_type {
        request = request.set("Content-Type", content_type);
    }

    let started = Instant::now();
    let outcome = if matches!(prepared.body, PreparedBody::None) {
        request.call()
    } else {
        request.send_bytes(&payload)
    };
    let response = match outcome {
        Ok(response) => response,
        // 4xx and 5xx are answers, not failures.
        Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(transport)) => return Err(transport_error(&transport)),
    };

    let status = response.status();
    let reason = response.status_text().to_owned();
    let final_url = response.get_url().to_owned();
    let mut headers = Vec::new();
    for name in response.headers_names() {
        for value in response.all(&name) {
            headers.push((name.clone(), value.to_owned()));
        }
    }
    let mut body = Vec::new();
    let mut reader = response.into_reader().take(options.max_body as u64 + 1);
    reader
        .read_to_end(&mut body)
        .map_err(|e| ApiError::new(format!("The response ended early: {e}")))?;
    let truncated = body.len() > options.max_body;
    body.truncate(options.max_body);
    Ok(Response {
        status,
        reason,
        headers,
        body,
        truncated,
        elapsed: started.elapsed(),
        url: final_url,
    })
}

fn transport_error(error: &ureq::Transport) -> ApiError {
    use ureq::ErrorKind;
    let detail = error.message().unwrap_or("");
    let text = match error.kind() {
        ErrorKind::Dns => format!("Could not find the host. {detail}"),
        ErrorKind::ConnectionFailed => format!("Could not connect. {detail}"),
        ErrorKind::TooManyRedirects => "Too many redirects.".to_owned(),
        ErrorKind::InvalidUrl | ErrorKind::UnknownScheme => {
            format!("The URL is not valid. {detail}")
        }
        ErrorKind::Io
            if detail.contains("timed out") || error.to_string().contains("timed out") =>
        {
            "The request timed out.".to_owned()
        }
        _ => error.to_string(),
    };
    ApiError::new(text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Answers one request with `reply` and hands back what it received.
    fn serve(reply: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                received.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&received).into_owned();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if received.len() >= split + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&received).into_owned()
        });
        (url, handle)
    }

    fn prepared(method: &str, url: &str) -> Prepared {
        Prepared {
            method: method.into(),
            url: url.into(),
            headers: vec![("X-Test".into(), "yes".into())],
            body: PreparedBody::None,
            missing: Vec::new(),
            empty: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn a_json_response_is_read_and_a_404_is_an_answer() {
        let (url, server) = serve(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"error\":\"no\"}",
        );
        let response = send(
            &prepared("GET", &format!("{url}/missing")),
            &SendOptions::default(),
        )
        .unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(response.reason, "Not Found");
        assert!(response.is_json() && !response.is_success());
        assert!(response
            .pretty_json()
            .unwrap()
            .contains("\"error\": \"no\""));
        let received = server.join().unwrap();
        assert!(received.starts_with("GET /missing HTTP/1.1"));
        assert!(received.to_ascii_lowercase().contains("x-test: yes"));
    }

    #[test]
    fn a_body_is_sent_and_a_long_response_is_cut() {
        let (url, server) =
            serve("HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789");
        let mut p = prepared("POST", &url);
        p.body = PreparedBody::Bytes(b"hello".to_vec());
        let options = SendOptions {
            max_body: 4,
            ..SendOptions::default()
        };
        let response = send(&p, &options).unwrap();
        assert_eq!(response.body, b"0123");
        assert!(response.truncated);
        let received = server.join().unwrap();
        assert!(received.starts_with("POST / HTTP/1.1"));
        assert!(received.ends_with("hello"));
    }

    #[test]
    fn the_failures_a_user_can_fix_say_what_to_fix() {
        let options = SendOptions::default();
        assert!(send(&prepared("GET", ""), &options)
            .unwrap_err()
            .to_string()
            .contains("URL"));
        assert!(send(&prepared("GET", "example.com/x"), &options)
            .unwrap_err()
            .to_string()
            .contains("https://example.com/x"));
        let mut p = prepared("GET", "http://{{host}}/");
        p.missing = vec!["host".into()];
        assert!(send(&p, &options)
            .unwrap_err()
            .to_string()
            .contains("{{host}}"));
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        assert!(send(&prepared("GET", &url), &options)
            .unwrap_err()
            .to_string()
            .contains("connect"));
    }

    #[test]
    fn multipart_bodies_carry_text_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "FILEDATA").unwrap();
        let parts = vec![
            MultipartPart {
                name: "t".into(),
                content: PartContent::Text("v".into()),
                content_type: None,
            },
            MultipartPart {
                name: "f".into(),
                content: PartContent::File(file),
                content_type: Some("text/plain".into()),
            },
        ];
        let (body, content_type) = multipart_body(&parts).unwrap();
        let boundary = content_type.split("boundary=").nth(1).unwrap();
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("name=\"t\"\r\n\r\nv\r\n"));
        assert!(text.contains(
            "name=\"f\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nFILEDATA\r\n"
        ));
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
        let missing = vec![MultipartPart {
            name: "x".into(),
            content: PartContent::File("/nope/x".into()),
            content_type: None,
        }];
        assert!(multipart_body(&missing)
            .unwrap_err()
            .to_string()
            .contains("/nope/x"));
    }
}
