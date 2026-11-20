//! Finds the address a dev server says it is listening on, so the UI can
//! offer to open it in a browser. Only this machine's addresses
//! count: a run's output is never a reason to open a remote page.
use regex::Regex;
use std::sync::OnceLock;

const HOST: &str = r"localhost|[a-z0-9-]+\.localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]";

struct Patterns {
    /// `http://127.0.0.1:8000/admin`
    url: Regex,
    /// `localhost:3000`, not part of a longer name or a URL.
    address: Regex,
    /// `Listening on port 5173`, `server started, port=8080`
    port: Regex,
    /// `Connecting to database server on port 5432`: somebody else's port.
    client: Regex,
    /// `listening on :8080`, `Listening at *:9000`
    any_host: Regex,
}

fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            Some(Patterns {
                url: Regex::new(&format!(
                    r#"(?i)\b(https?)://({HOST})(?::(\d{{1,5}}))?(/[^\s"'<>()\[\]]*)?"#
                ))
                .ok()?,
                address: Regex::new(&format!(r"(?i)(?:^|[^\w./@:-])({HOST}):(\d{{2,5}})\b")).ok()?,
                port: Regex::new(
                    r"(?i)\b(?:listening|listens|serving|serves|running|started|ready|available)\b.*?\bport\s*[:=]?\s*(\d{2,5})\b",
                )
                .ok()?,
                client: Regex::new(
                    r"(?i)\b(?:connect(?:ing|ed|ion|s)?|waiting|wait|retry(?:ing)?|unreachable|refused|failed|cannot|could not|unable)\b",
                )
                .ok()?,
                any_host: Regex::new(
                    r"(?i)\b(?:listening|serving|running|started)\s+(?:on|at)\s+(?:\*|tcp)?\s*:(\d{2,5})\b",
                )
                .ok()?,
            })
        })
        .as_ref()
}

fn valid_port(port: &str) -> bool {
    port.parse::<u32>()
        .is_ok_and(|port| (1..=65535).contains(&port))
}

/// A browser cannot open the "every interface" addresses a server binds.
fn reachable(host: &str) -> String {
    match host {
        "0.0.0.0" => "127.0.0.1".into(),
        "[::]" => "[::1]".into(),
        host => host.to_ascii_lowercase(),
    }
}

fn line_urls(line: &str, patterns: &Patterns, found: &mut Vec<String>) {
    let mut add = |url: String| {
        if !found.contains(&url) {
            found.push(url);
        }
    };
    let mut any = false;
    for captures in patterns.url.captures_iter(line) {
        let port = captures.get(3).map(|port| port.as_str());
        if port.is_some_and(|port| !valid_port(port)) {
            continue;
        }
        // `localhost.example.com` and a port with too many digits only
        // start like a local address.
        let mut after = line[captures.get(0).map_or(0, |all| all.end())..].chars();
        let longer = match after.next() {
            Some('.') => after.next().is_some_and(char::is_alphanumeric),
            Some(next) => next.is_alphanumeric() || next == '-',
            None => false,
        };
        if longer && captures.get(4).is_none() {
            continue;
        }
        let mut url = format!(
            "{}://{}",
            captures[1].to_ascii_lowercase(),
            reachable(&captures[2])
        );
        if let Some(port) = port {
            url.push(':');
            url.push_str(port);
        }
        if let Some(path) = captures.get(4) {
            // Sentence punctuation after an address is not part of it.
            url.push_str(
                path.as_str()
                    .trim_end_matches(['.', ',', ';', ':', '!', '?']),
            );
        }
        add(url);
        any = true;
    }
    if any {
        return;
    }
    for captures in patterns.address.captures_iter(line) {
        if valid_port(&captures[2]) {
            add(format!(
                "http://{}:{}",
                reachable(&captures[1]),
                &captures[2]
            ));
            any = true;
        }
    }
    if any {
        return;
    }
    // A sentence about a port is only an announcement when the program says
    // it is the one listening; a client naming the port it dials is not.
    if patterns.client.is_match(line) {
        return;
    }
    let port = patterns
        .any_host
        .captures(line)
        .or_else(|| patterns.port.captures(line));
    if let Some(captures) = port.filter(|captures| valid_port(&captures[1])) {
        add(format!("http://localhost:{}", &captures[1]));
    }
}

/// The local addresses `text` announces, in order and without repeats. A
/// line with a full URL gives that; otherwise a bare `localhost:3000`;
/// otherwise a "listening on port N" sentence gives `http://localhost:N`.
/// The text must be free of terminal escape codes (the registry's output
/// tail is).
pub fn local_urls(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(patterns) = patterns() {
        for line in text.lines() {
            line_urls(line, patterns, &mut found);
        }
    }
    found
}

/// The first of `local_urls`.
pub fn first_local_url(text: &str) -> Option<String> {
    local_urls(text).into_iter().next()
}

/// This machine's own addresses on its networks, so that a dev server
/// reached through one of them counts as local. Read anew on each call: they
/// change when the machine joins another network.
pub fn machine_ips() -> Vec<std::net::IpAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| interface.ip())
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses_are_found() {
        let cases: [(&str, &[&str]); 29] = [
            (
                "Starting development server at http://127.0.0.1:8000/",
                &["http://127.0.0.1:8000/"],
            ),
            (
                "  ➜  Local:   http://localhost:5173/",
                &["http://localhost:5173/"],
            ),
            (
                "ready - started server on 0.0.0.0:3000, url: http://localhost:3000",
                &["http://localhost:3000"],
            ),
            (
                "Uvicorn running on http://0.0.0.0:8000 (Press CTRL+C to quit)",
                &["http://127.0.0.1:8000"],
            ),
            (
                " * Running on https://127.0.0.1:5000",
                &["https://127.0.0.1:5000"],
            ),
            (
                "Open http://localhost:3000/docs.",
                &["http://localhost:3000/docs"],
            ),
            (
                "see (http://localhost:8080/a/b?x=1) for more",
                &["http://localhost:8080/a/b?x=1"],
            ),
            ("HTTP://LOCALHOST:8080", &["http://localhost:8080"]),
            (
                "http://[::1]:4000/ and http://[::]:4001",
                &["http://[::1]:4000/", "http://[::1]:4001"],
            ),
            ("http://app.localhost:3000", &["http://app.localhost:3000"]),
            ("http://localhost", &["http://localhost"]),
            ("server at localhost:3000", &["http://localhost:3000"]),
            ("127.0.0.1:8000", &["http://127.0.0.1:8000"]),
            (
                "bound to 0.0.0.0:9000 and localhost:9001",
                &["http://127.0.0.1:9000", "http://localhost:9001"],
            ),
            ("Listening on port 5173", &["http://localhost:5173"]),
            ("Server started, port=8080", &["http://localhost:8080"]),
            ("app is running on port: 4200", &["http://localhost:4200"]),
            ("listening on :8080", &["http://localhost:8080"]),
            ("Listening at *:9000", &["http://localhost:9000"]),
            // Not this machine, not an address, not a port.
            ("Docs: https://example.com:8080/guide", &[]),
            ("fetching http://localhost.example.com:80/x", &[]),
            (
                "user@localhost:22 mylocalhost:3000 db.localhost.internal:5432",
                &[],
            ),
            ("http://localhost:99999 localhost:0 port 7 is closed", &[]),
            ("12 tests passed in 3.21s on report 8080", &[]),
            // A port somebody else listens on, or a server with no verb.
            ("Connecting to database server on port 5432", &[]),
            ("Connected to redis running on port 6379", &[]),
            ("Waiting for the server to be ready on port 9200", &[]),
            ("postgres server port 5432", &[]),
            ("connection refused: nothing listening on :8080", &[]),
        ];
        for (text, expected) in cases {
            assert_eq!(local_urls(text), expected, "{text}");
        }
    }

    #[test]
    fn addresses_are_listed_once_across_lines() {
        let text = "Local: http://localhost:5173/\nNetwork: http://192.168.1.5:5173/\n\
                    again http://localhost:5173/\nListening on port 9229\n";
        assert_eq!(
            local_urls(text),
            ["http://localhost:5173/", "http://localhost:9229"]
        );
        assert_eq!(
            first_local_url(text).as_deref(),
            Some("http://localhost:5173/")
        );
        assert_eq!(first_local_url("nothing here"), None);
    }

    #[test]
    fn the_machines_addresses_leave_out_loopback_and_every_interface() {
        for ip in machine_ips() {
            assert!(!ip.is_loopback() && !ip.is_unspecified(), "{ip}");
        }
    }
}
