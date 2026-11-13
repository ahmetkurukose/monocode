//! The browser pane's rules that need no web view: what the address field's
//! text means. See docs/native/browser.md.

/// Hosts that are this machine or its network, reached over plain http
/// unless the user says otherwise.
pub fn local_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host == "::1"
        || host.parse::<std::net::Ipv4Addr>().is_ok()
}

/// Hosts that can only be this machine or its own network: a certificate
/// from one of them is a development server's, signed by itself.
pub fn private_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => false,
    }
}

fn host_of(text: &str) -> &str {
    let end = text.find(['/', '?', '#']).unwrap_or(text.len());
    let authority = &text[..end];
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                host
            } else {
                authority
            }
        })
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// What to load for text typed in the address field: an address as it is, a
/// bare host with a scheme put in front (http for local ones), and anything
/// else as a web search.
pub fn address(input: &str) -> String {
    let text = input.trim();
    if text.is_empty() {
        return "about:blank".into();
    }
    let lower = text.to_ascii_lowercase();
    if ["http://", "https://", "file://", "about:", "data:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        return text.into();
    }
    let host = host_of(text);
    let looks_like_host = !text.contains(char::is_whitespace)
        && !host.is_empty()
        && (local_host(host)
            || (host.contains('.') && !host.starts_with('.') && !host.ends_with('.')));
    if looks_like_host {
        let scheme = if local_host(host) { "http" } else { "https" };
        return format!("{scheme}://{text}");
    }
    format!("https://www.google.com/search?q={}", encode(text))
}

/// User agents to try a page as another browser or device. The first is
/// the default: no override, WebKit's own string with Safari's version.
/// The versions here are a snapshot (November 2026) and want refreshing now
/// and then; a server sees exactly these strings, the engine stays WebKit.
pub const USER_AGENTS: &[(&str, &str)] = &[
    ("Safari (Mac)", ""),
    ("Chrome (Mac)", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36"),
    ("Chrome (Windows)", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36"),
    ("Firefox (Mac)", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:145.0) Gecko/20100101 Firefox/145.0"),
    ("Edge (Windows)", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36 Edg/156.0.0.0"),
    ("Safari (iPhone)", "Mozilla/5.0 (iPhone; CPU iPhone OS 26_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.1 Mobile/15E148 Safari/604.1"),
    ("Safari (iPad)", "Mozilla/5.0 (iPad; CPU OS 26_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.1 Mobile/15E148 Safari/604.1"),
    ("Chrome (Android)", "Mozilla/5.0 (Linux; Android 16; Pixel 10) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Mobile Safari/537.36"),
    ("Googlebot", "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"),
];

/// Sizes to hold a page at, to see it as a device does: name, width, height.
pub const DEVICE_SIZES: &[(&str, u32, u32)] = &[
    ("iPhone", 390, 844),
    ("iPhone Pro Max", 440, 956),
    ("Android phone", 412, 915),
    ("iPad", 820, 1180),
    ("Laptop", 1280, 800),
];

/// The user agent string for a preset's name or for a string typed in full.
/// `None` is the default: an empty or unknown-looking choice, or the first
/// preset.
pub fn user_agent(choice: &str) -> Option<String> {
    let choice = choice.trim();
    if choice.is_empty() || choice.eq_ignore_ascii_case("default") {
        return None;
    }
    if let Some((_, agent)) = USER_AGENTS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(choice))
    {
        return (!agent.is_empty()).then(|| (*agent).to_owned());
    }
    Some(choice.to_owned())
}

/// The preset a user agent string is, for showing it by name.
pub fn user_agent_name(agent: Option<&str>) -> String {
    match agent {
        None => USER_AGENTS[0].0.to_owned(),
        Some(agent) => USER_AGENTS
            .iter()
            .find(|(_, preset)| *preset == agent)
            .map_or_else(|| "Custom".to_owned(), |(name, _)| (*name).to_owned()),
    }
}

/// A loaded page, as the rules that free memory see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageLoad {
    pub id: i64,
    /// Seconds since the page was last on screen or used by an agent.
    pub idle_secs: u64,
    pub on_screen: bool,
    /// The user asked for this page to stay loaded.
    pub keep_awake: bool,
}

/// A page used this recently is never put to sleep to make room.
const RECENT_SECS: u64 = 120;

/// Which loaded pages to put to sleep now. A page on screen never sleeps.
/// - `pressure`: the system is short of memory; every page off screen goes,
///   kept awake or not.
/// - `idle_limit_secs`: a page off screen that long sleeps (`None`: never).
/// - `max_live`: with more pages loaded than this, the ones idle longest
///   sleep until the rest fit, sparing those used in the last two minutes.
pub fn pages_to_sleep(
    pages: &[PageLoad],
    idle_limit_secs: Option<u64>,
    max_live: usize,
    pressure: bool,
) -> Vec<i64> {
    let mut sleeping: Vec<i64> = Vec::new();
    for page in pages.iter().filter(|page| !page.on_screen) {
        let idle = idle_limit_secs.is_some_and(|limit| page.idle_secs >= limit);
        if pressure || (idle && !page.keep_awake) {
            sleeping.push(page.id);
        }
    }
    let mut spare: Vec<&PageLoad> = (pages.iter())
        .filter(|page| {
            !page.on_screen
                && !page.keep_awake
                && page.idle_secs >= RECENT_SECS
                && !sleeping.contains(&page.id)
        })
        .collect();
    spare.sort_by_key(|page| std::cmp::Reverse(page.idle_secs));
    let mut live = pages.len() - sleeping.len();
    for page in spare {
        if live <= max_live.max(1) {
            break;
        }
        sleeping.push(page.id);
        live -= 1;
    }
    sleeping
}

/// A page's name for a tab or a row before it has a title.
pub fn label(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if host.is_empty() {
        "New page".into()
    } else {
        host.trim_start_matches("www.").into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_is_an_address_a_host_or_a_search() {
        assert_eq!(
            address("https://example.com/a?b"),
            "https://example.com/a?b"
        );
        assert_eq!(address("  HTTP://Example.com "), "HTTP://Example.com");
        assert_eq!(address("example.com"), "https://example.com");
        assert_eq!(
            address("github.com/zed-industries/zed"),
            "https://github.com/zed-industries/zed"
        );
        // This machine and its network are plain http.
        assert_eq!(address("localhost:3000"), "http://localhost:3000");
        assert_eq!(address("localhost"), "http://localhost");
        assert_eq!(
            address("127.0.0.1:8080/health"),
            "http://127.0.0.1:8080/health"
        );
        assert_eq!(address("app.localhost:5173"), "http://app.localhost:5173");
        assert_eq!(address("printer.local"), "http://printer.local");
        assert_eq!(address("[::1]:9000"), "http://[::1]:9000");
        // Words are a search.
        assert_eq!(
            address("rust objc2 wkwebview"),
            "https://www.google.com/search?q=rust+objc2+wkwebview"
        );
        assert_eq!(
            address("what is 1+1?"),
            "https://www.google.com/search?q=what+is+1%2B1%3F"
        );
        assert_eq!(address("readme"), "https://www.google.com/search?q=readme");
        assert_eq!(address("  "), "about:blank");
        assert_eq!(address("about:blank"), "about:blank");
    }

    #[test]
    fn only_this_machine_and_its_network_are_private() {
        for host in [
            "localhost",
            "app.localhost",
            "nas.local",
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.20",
            "::1",
            "[::1]",
        ] {
            assert!(private_host(host), "{host}");
        }
        for host in [
            "example.com",
            "8.8.8.8",
            "localhost.example.com",
            "mylocal",
            "2001:db8::1",
        ] {
            assert!(!private_host(host), "{host}");
        }
    }

    #[test]
    fn a_user_agent_is_a_preset_by_name_or_any_string() {
        assert_eq!(user_agent(""), None);
        assert_eq!(user_agent("default"), None);
        assert_eq!(user_agent("Safari (Mac)"), None);
        assert!(user_agent("chrome (android)").unwrap().contains("Android"));
        assert_eq!(user_agent("MyBot/1.0").as_deref(), Some("MyBot/1.0"));
        assert_eq!(user_agent_name(None), "Safari (Mac)");
        assert_eq!(
            user_agent_name(user_agent("Googlebot").as_deref()),
            "Googlebot"
        );
        assert_eq!(user_agent_name(Some("MyBot/1.0")), "Custom");
        // Every preset but the default carries a string, and names are unique.
        assert!(USER_AGENTS[1..].iter().all(|(_, agent)| !agent.is_empty()));
        let mut names: Vec<_> = USER_AGENTS
            .iter()
            .map(|(name, _)| name.to_lowercase())
            .collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), USER_AGENTS.len());
    }

    #[test]
    fn pages_sleep_when_idle_when_there_are_too_many_and_under_pressure() {
        let page = |id, idle_secs, on_screen, keep_awake| PageLoad {
            id,
            idle_secs,
            on_screen,
            keep_awake,
        };
        let minutes = |m: u64| m * 60;
        // Idle: off screen for the limit, unless kept awake or on screen.
        let pages = [
            page(1, minutes(20), false, false),
            page(2, minutes(20), false, true),
            page(3, minutes(20), true, false),
            page(4, minutes(2), false, false),
        ];
        assert_eq!(pages_to_sleep(&pages, Some(minutes(15)), 10, false), [1]);
        assert!(pages_to_sleep(&pages, None, 10, false).is_empty());
        // Too many: the longest idle go first, until the rest fit. A page
        // just used, on screen or kept awake is not counted out.
        let pages = [
            page(1, minutes(3), false, false),
            page(2, minutes(9), false, false),
            page(3, minutes(6), false, false),
            page(4, 30, false, false),
            page(5, 0, true, false),
        ];
        assert_eq!(pages_to_sleep(&pages, None, 3, false), [2, 3]);
        assert_eq!(pages_to_sleep(&pages, None, 4, false), [2]);
        assert!(pages_to_sleep(&pages, None, 5, false).is_empty());
        // Nothing to spare: the limit is passed rather than a fresh page lost.
        let fresh = [
            page(1, 10, false, false),
            page(2, 20, false, false),
            page(3, 0, true, false),
        ];
        assert!(pages_to_sleep(&fresh, None, 1, false).is_empty());
        // Short of memory: every page off screen, kept awake or not.
        let pages = [
            page(1, 5, false, true),
            page(2, 5, false, false),
            page(3, 5, true, false),
        ];
        assert_eq!(pages_to_sleep(&pages, None, 10, true), [1, 2]);
    }

    #[test]
    fn a_page_without_a_title_is_named_after_its_host() {
        assert_eq!(label("https://www.example.com/a/b"), "example.com");
        assert_eq!(label("http://localhost:3000/"), "localhost:3000");
        assert_eq!(label("about:blank"), "about:blank");
        assert_eq!(label(""), "New page");
    }
}
