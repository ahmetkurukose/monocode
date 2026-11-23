//! Chrome's match patterns (`<scheme>://<host><path>`, `<all_urls>`) and
//! globs, for content scripts, host permissions, web-accessible resources,
//! `externally_connectable` and `tabs.query({url})`.
//!
//! As in Chrome: a `*` scheme is http or https; `*.example.com` is
//! example.com and every host under it; a pattern without a port matches
//! any port; the path glob matches the URL's path with its query.

use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Scheme {
    /// `*`: http or https.
    Web,
    /// `<all_urls>`.
    All,
    Is(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Host {
    Any,
    /// `*.example.com`: example.com and its subdomains.
    Under(String),
    Is(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchPattern {
    scheme: Scheme,
    host: Host,
    /// `None`: any port.
    port: Option<u16>,
    path: String,
    source: String,
}

/// Schemes `<all_urls>` covers.
const ALL_SCHEMES: &[&str] = &["http", "https", "ws", "wss", "file"];

impl MatchPattern {
    /// `None` for a pattern Chrome would refuse.
    pub fn parse(pattern: &str) -> Option<Self> {
        let source = pattern.to_owned();
        if pattern == "<all_urls>" {
            return Some(MatchPattern {
                scheme: Scheme::All,
                host: Host::Any,
                port: None,
                path: "/*".into(),
                source,
            });
        }
        let (scheme, rest) = pattern.split_once("://")?;
        let scheme = match scheme {
            "*" => Scheme::Web,
            "http" | "https" | "ws" | "wss" | "file" | "ftp" | "urn" => {
                Scheme::Is(scheme.to_owned())
            }
            _ => return None,
        };
        let at = rest.find('/')?;
        let (authority, path) = (&rest[..at], &rest[at..]);
        let (host, port) = if scheme == Scheme::Is("file".into()) {
            (Host::Any, None)
        } else {
            let (host, port) = match authority.rsplit_once(':') {
                Some((h, p)) if !h.ends_with(']') || authority.starts_with('[') => (h, Some(p)),
                _ => (authority, None),
            };
            let port = match port {
                None | Some("*") => None,
                Some(p) => Some(p.parse::<u16>().ok()?),
            };
            let host = host.to_ascii_lowercase();
            let host = if host == "*" {
                Host::Any
            } else if let Some(base) = host.strip_prefix("*.") {
                if base.is_empty() || base.contains('*') {
                    return None;
                }
                Host::Under(base.to_owned())
            } else if host.is_empty() || host.contains('*') {
                return None;
            } else {
                Host::Is(host)
            };
            (host, port)
        };
        Some(MatchPattern {
            scheme,
            host,
            port,
            path: path.to_owned(),
            source,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Whether the pattern covers every web site (host warnings).
    pub fn covers_all_hosts(&self) -> bool {
        self.host == Host::Any
    }

    pub fn matches(&self, url: &str) -> bool {
        Url::parse(url).is_ok_and(|url| self.matches_url(&url))
    }

    /// The pattern as a JavaScript regular expression's source (flag `i`),
    /// for scripts that test `location.href` in the page.
    pub fn regex_source(&self) -> String {
        let scheme = match &self.scheme {
            Scheme::Web => "https?".to_owned(),
            Scheme::All => "(https?|wss?|file)".to_owned(),
            Scheme::Is(s) => regex_escape(s),
        };
        if self.scheme == Scheme::All {
            return format!("^{scheme}:");
        }
        if self.scheme == Scheme::Is("file".into()) {
            return format!("^file://{}$", glob_regex(&self.path));
        }
        let host = match &self.host {
            Host::Any => "[^/:]*".to_owned(),
            Host::Under(base) => format!("([^/:]*\\.)?{}", regex_escape(base)),
            Host::Is(h) => regex_escape(h),
        };
        let port = match self.port {
            Some(p) => format!(":{p}"),
            None => "(:\\d+)?".to_owned(),
        };
        format!("^{scheme}://{host}{port}{}(#.*)?$", glob_regex(&self.path))
    }

    pub fn matches_url(&self, url: &Url) -> bool {
        let scheme = url.scheme();
        let scheme_ok = match &self.scheme {
            Scheme::Web => scheme == "http" || scheme == "https",
            Scheme::All => ALL_SCHEMES.contains(&scheme),
            Scheme::Is(s) => s == scheme,
        };
        if !scheme_ok {
            return false;
        }
        if scheme != "file" {
            let host = url.host_str().unwrap_or("").to_ascii_lowercase();
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let host_ok = match &self.host {
                Host::Any => true,
                Host::Is(h) => h.trim_start_matches('[').trim_end_matches(']') == host,
                Host::Under(base) => host == base || host.ends_with(&format!(".{base}")),
            };
            if !host_ok {
                return false;
            }
            if let Some(port) = self.port {
                if url.port_or_known_default() != Some(port) {
                    return false;
                }
            }
        }
        let mut path = url.path().to_owned();
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        glob(&self.path, &path)
    }
}

/// A list of patterns; unreadable ones are dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatternSet(pub Vec<MatchPattern>);

impl PatternSet {
    pub fn parse<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Self {
        PatternSet(
            patterns
                .into_iter()
                .filter_map(MatchPattern::parse)
                .collect(),
        )
    }

    pub fn matches(&self, url: &str) -> bool {
        match Url::parse(url) {
            Ok(url) => self.0.iter().any(|p| p.matches_url(&url)),
            Err(_) => false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn sources(&self) -> Vec<String> {
        self.0.iter().map(|p| p.source.clone()).collect()
    }
}

fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\^$.|?*+()[]{}/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A match pattern's path glob (`*` only) as regex source.
fn glob_regex(glob: &str) -> String {
    glob.split('*')
        .map(regex_escape)
        .collect::<Vec<_>>()
        .join(".*")
}

/// A content script glob (`*` and `?`) as regex source over a whole URL.
pub fn glob_regex_source(glob: &str) -> String {
    let mut out = String::from("^");
    for c in glob.chars() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            c => out.push_str(&regex_escape(&c.to_string())),
        }
    }
    out.push('$');
    out
}

/// `*` (any run, `/` included) and `?` (one character) over `text`.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// The origin of `url` (`scheme://host[:port]`), `None` for an opaque one.
pub fn origin_of(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    match parsed.origin() {
        url::Origin::Tuple(..) => Some(parsed.origin().ascii_serialization()),
        // Custom schemes are opaque to the url crate: scheme and host.
        url::Origin::Opaque(_) => {
            let host = parsed.host_str()?;
            Some(format!("{}://{host}", parsed.scheme()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, url: &str) -> bool {
        MatchPattern::parse(pattern).unwrap().matches(url)
    }

    #[test]
    fn schemes_hosts_and_ports() {
        assert!(m("https://*/*", "https://example.com/"));
        assert!(!m("https://*/*", "http://example.com/"));
        assert!(m("*://*/*", "http://example.com/a"));
        assert!(!m("*://*/*", "file:///etc/hosts"));
        assert!(!m("*://*/*", "mcext://abc/popup.html"));
        assert!(m("<all_urls>", "file:///etc/hosts"));
        assert!(m("<all_urls>", "wss://x.io/socket"));
        assert!(!m("<all_urls>", "chrome://settings/"));
        assert!(m("https://*.proton.me/*", "https://proton.me/"));
        assert!(m(
            "https://*.proton.me/*",
            "https://account.proton.me/login"
        ));
        assert!(!m("https://*.proton.me/*", "https://notproton.me/"));
        assert!(m(
            "https://account.proton.me/*",
            "https://ACCOUNT.proton.me/x"
        ));
        assert!(m("http://localhost/*", "http://localhost:3000/"));
        assert!(m("http://localhost:3000/*", "http://localhost:3000/"));
        assert!(!m("http://localhost:3000/*", "http://localhost:4000/"));
        assert!(m("http://localhost:*/*", "http://localhost:4000/"));
        assert!(m("http://127.0.0.1/*", "http://127.0.0.1:18765/login.html"));
    }

    #[test]
    fn paths_include_the_query() {
        assert!(m("https://a.com/login*", "https://a.com/login?next=/"));
        assert!(!m("https://a.com/login", "https://a.com/login?x"));
        assert!(m("https://a.com/*/edit", "https://a.com/x/y/edit"));
        assert!(m("https://pass.proton.me/*", "https://pass.proton.me/"));
    }

    #[test]
    fn invalid_patterns_are_refused() {
        for bad in [
            "https://",
            "https://a*.com/",
            "chrome://x/",
            "nohost",
            "https://*./x",
            "http://a:99999/",
        ] {
            assert!(MatchPattern::parse(bad).is_none(), "{bad}");
        }
        let set = PatternSet::parse(["bad", "https://*/*"]);
        assert_eq!(set.0.len(), 1);
        assert!(set.matches("https://x.y/"));
        assert!(!set.matches("not a url"));
    }

    #[test]
    fn page_side_regexes_agree_with_matching() {
        let patterns = [
            "https://*/*",
            "*://*/*",
            "<all_urls>",
            "https://*.proton.me/*",
            "http://localhost:3000/*",
            "https://a.com/login*",
            "file:///tmp/*",
        ];
        let urls = [
            "https://example.com/",
            "http://example.com/a?b=1",
            "https://proton.me/x",
            "https://account.proton.me/login#frag",
            "https://notproton.me/",
            "http://localhost:3000/",
            "http://localhost:4000/",
            "https://a.com/login?next=1",
            "https://a.com/logout",
            "file:///tmp/x.html",
            "mcext://abc/popup.html",
            "about:blank",
            "wss://x.io/",
        ];
        for p in patterns {
            let parsed = MatchPattern::parse(p).unwrap();
            let re = regex::RegexBuilder::new(&parsed.regex_source())
                .case_insensitive(true)
                .build()
                .unwrap();
            for u in urls {
                assert_eq!(
                    re.is_match(u),
                    parsed.matches(u),
                    "{p} on {u}: {}",
                    parsed.regex_source()
                );
            }
        }
        let g = regex::Regex::new(&glob_regex_source("https://*.a.com/?x*")).unwrap();
        assert!(g.is_match("https://w.a.com/1x2"));
        assert!(!g.is_match("https://w.a.com/12"));
    }

    #[test]
    fn globs() {
        assert!(glob("assets/*.png", "assets/a/b.png"));
        assert!(glob("*.wasm", "x/y.wasm"));
        assert!(!glob("assets/*.png", "assets/a.svg"));
        assert!(glob("a?c", "abc"));
        assert!(glob("*", ""));
        assert!(glob("dropdown.html", "dropdown.html"));
    }

    #[test]
    fn origins() {
        assert_eq!(
            origin_of("https://a.com:443/x").as_deref(),
            Some("https://a.com")
        );
        assert_eq!(
            origin_of("http://localhost:3000/").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            origin_of("mcext://abc/popup.html").as_deref(),
            Some("mcext://abc")
        );
        assert_eq!(origin_of("about:blank"), None);
    }
}
