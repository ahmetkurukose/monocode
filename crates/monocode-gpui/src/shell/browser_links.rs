//! Where a web link clicked in the app opens (K19): a local address, such
//! as a dev server's, in Monocode's browser; anything else in the user's own
//! browser, where they are signed in. "Open all links in Monocode" sends
//! every web link to Monocode, and ⇧-click opens a link in the other one.
//! Links that need the user's real browser session (sign-ins, GitHub's pull
//! request pages) call `cx.open_url` themselves and never come here.
//!
//! Link routing (docs/native/modes.md §Link routing), as SOCode's Air
//! Traffic Control: the user's rules, in order, send a site to Browser
//! mode, a project, the Little window or the system browser before K19's
//! rule decides. Links other apps hand to Monocode open in the Little window
//! unless a rule says otherwise.
use super::Shell;
use gpui::{AnyWindowHandle, App, Context, Global, Window};
use serde::{Deserialize, Serialize};
use socode_browser::routing::LinkMatch;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `url` opens in Monocode's browser. `machine` holds this
/// machine's own network addresses, which count as local. Only http(s)
/// links ever do: `mailto:`, `file://` and the like go to the system even
/// when inverted.
pub(crate) fn opens_in_app(url: &str, all_links: bool, invert: bool, machine: &[IpAddr]) -> bool {
    let Some(host) = web_host(url) else {
        return false;
    };
    (all_links || local_host(&host, machine)) != invert
}

/// The lowercased host of an http(s) URL, without brackets or a port.
/// `None` for any other scheme or a URL without a host.
pub(crate) fn web_host(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or(rest);
    // `http://localhost@example.com` is example.com, signed in as "localhost".
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or(bracketed)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    // `localhost.` is the same name, written fully qualified.
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// This machine: its loopback and "every interface" addresses, the names
/// that resolve to it or its network (`*.localhost`, `*.local`), and its own
/// network addresses.
fn local_host(host: &str, machine: &[IpAddr]) -> bool {
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    let ip = match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        Ok(ip) => ip,
        Err(_) => return false,
    };
    ip.is_loopback()
        || ip == IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        || ip == IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        || machine.contains(&ip)
}

/// Where a rule sends the links it matches (user decision: no Spaces).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LinkTarget {
    /// A tab at the top of Browser mode's Today.
    Browser,
    /// A browser page of the project at this path.
    Project(String),
    /// The Little window.
    Little,
    /// The user's own browser.
    System,
}

/// One rule: links that match `condition` open at `target`. Rules are
/// tried in their order; the first enabled one that matches wins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LinkRoute {
    pub id: u64,
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub condition: LinkMatch,
    pub target: LinkTarget,
}

fn enabled() -> bool {
    true
}

/// A rule matching `value` the way `kind` says ("domain", "contains",
/// "equal"); `None` for an empty value.
pub(crate) fn link_match(kind: &str, value: &str) -> Option<LinkMatch> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(match kind {
        "contains" => LinkMatch::Contains(value.to_owned()),
        "equal" => LinkMatch::IsEqualTo(value.to_owned()),
        // A pasted address names its site.
        _ => LinkMatch::Domain(web_host(value).unwrap_or_else(|| value.to_ascii_lowercase())),
    })
}

/// `routes` with a new rule at their end; returns its id.
pub(crate) fn add_route(
    routes: &mut Vec<LinkRoute>,
    condition: LinkMatch,
    target: LinkTarget,
) -> u64 {
    let id = routes.iter().map(|route| route.id).max().unwrap_or(0) + 1;
    routes.push(LinkRoute {
        id,
        enabled: true,
        condition,
        target,
    });
    id
}

/// Where a link opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Opens {
    /// K19's "in Monocode": a page of the project of the window it was
    /// clicked in (Browser mode shows it there when it is on screen).
    Here,
    Browser,
    Project(String),
    Little,
    System,
}

impl From<LinkTarget> for Opens {
    fn from(target: LinkTarget) -> Self {
        match target {
            LinkTarget::Browser => Opens::Browser,
            LinkTarget::Project(path) => Opens::Project(path),
            LinkTarget::Little => Opens::Little,
            LinkTarget::System => Opens::System,
        }
    }
}

/// Where `url` opens. Non-web links always go to the system. The first
/// enabled rule that matches decides; without one, a link from another app
/// (`external`) opens in the Little window and a click in Monocode follows
/// K19 (`opens_in_app`). ⇧-click (`invert`) opens what would go to the
/// system browser in Monocode, and anything else in the system browser.
/// A link from another app never goes back to the system browser, which may
/// be Monocode itself: there the Little window stands in.
pub(crate) fn route(
    url: &str,
    routes: &[LinkRoute],
    all_links: bool,
    invert: bool,
    external: bool,
    machine: &[IpAddr],
) -> Opens {
    if web_host(url).is_none() {
        return Opens::System;
    }
    let parsed = url::Url::parse(url.trim()).ok();
    let ruled = (routes.iter())
        .filter(|route| route.enabled)
        .find(|route| (parsed.as_ref()).is_some_and(|parsed| route.condition.matches(parsed, None)))
        .map(|route| Opens::from(route.target.clone()));
    let opens = ruled.unwrap_or_else(|| {
        if external {
            Opens::Little
        } else if opens_in_app(url, all_links, false, machine) {
            Opens::Here
        } else {
            Opens::System
        }
    });
    let opens = match (invert, opens) {
        (false, opens) => opens,
        (true, Opens::System) => Opens::Here,
        (true, _) => Opens::System,
    };
    match opens {
        Opens::System if external => Opens::Little,
        opens => opens,
    }
}

/// `route` with the user's settings and this machine's addresses. Only
/// macOS has the browser pane: elsewhere every link goes to the system.
fn route_now(url: &str, invert: bool, external: bool, cx: &App) -> Opens {
    if !cfg!(target_os = "macos") {
        return Opens::System;
    }
    let preferences = cx.try_global::<crate::settings::Preferences>();
    let all_links = preferences.is_some_and(|preferences| preferences.browser_all_links);
    let routes = preferences.map_or(&[][..], |preferences| &preferences.browser_link_routes);
    route(
        url,
        routes,
        all_links,
        invert,
        external,
        &monocode_core::code::run::machine_ips(),
    )
}

/// Pages a click sent to Monocode's browser, waiting for a shell's next
/// frame, with where each opens. Each names the window it was clicked in,
/// so that window's shell opens it; `None` (a floating chat, which has no
/// shell, or another app) lets whichever shell draws first take it.
#[derive(Default)]
pub(crate) struct LinkQueue(pub(crate) Vec<(Option<AnyWindowHandle>, String, Opens)>);
impl Global for LinkQueue {}

/// Opens a clicked web link where `route` says. The one way in for every
/// view (a terminal, a reply), since most have no shell to call: a page for
/// Monocode's browser waits in `LinkQueue` for the shell of the window it
/// was clicked in.
pub(crate) fn open_link(url: &str, invert: bool, window: &Window, cx: &mut App) {
    match route_now(url, invert, false, cx) {
        Opens::System => cx.open_url(url),
        opens => {
            let owner =
                (window.root::<Shell>().flatten().is_some()).then(|| window.window_handle());
            (cx.default_global::<LinkQueue>().0).push((owner, url.to_owned(), opens));
            cx.refresh_windows();
        }
    }
}

/// Links another app handed to Monocode (it is their browser): each opens
/// where the rules say, else in the Little window. Other schemes are not
/// ours to open.
pub(crate) fn open_external(urls: Vec<String>, cx: &mut App) {
    for url in urls {
        if web_host(&url).is_none() {
            continue;
        }
        let opens = route_now(&url, false, true, cx);
        (cx.default_global::<LinkQueue>().0).push((None, url, opens));
    }
    cx.activate(true);
    cx.refresh_windows();
}

/// Takes the queued pages `window`'s shell should open: its own, those no
/// shell's window claimed, and those whose window closed before drawing.
pub(super) fn take_for(window: AnyWindowHandle, cx: &mut App) -> Vec<(String, Opens)> {
    let Some(queue) = cx.try_global::<LinkQueue>() else {
        return Vec::new();
    };
    if queue.0.is_empty() {
        return Vec::new();
    }
    let open = cx.windows();
    let mut taken = Vec::new();
    cx.global_mut::<LinkQueue>()
        .0
        .retain(|(owner, url, opens)| {
            let ours = owner.is_none_or(|owner| owner == window || !open.contains(&owner));
            if ours {
                taken.push((url.clone(), opens.clone()));
            }
            !ours
        });
    taken
}

/// The queued pages' addresses, for tests.
#[cfg(test)]
pub(crate) fn queued(cx: &App) -> Vec<String> {
    (cx.try_global::<LinkQueue>()).map_or_else(Vec::new, |queue| {
        queue.0.iter().map(|(_, url, _)| url.clone()).collect()
    })
}

/// Whether a page waits for some shell, so it should look on its next frame.
pub(super) fn pending(cx: &App) -> bool {
    cx.try_global::<LinkQueue>()
        .is_some_and(|queue| !queue.0.is_empty())
}

impl Shell {
    /// A queued link opens where it was routed. A rule's project that is no
    /// longer in the workspace falls back to the clicked window's project.
    pub(super) fn open_routed_link(
        &mut self,
        url: String,
        opens: Opens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match opens {
            Opens::Browser => {
                self.browse_new_tab(Some(url), None, true, window, cx);
            }
            Opens::Little => self.open_little(url, window, cx),
            Opens::System => cx.open_url(&url),
            Opens::Project(path) if self.workspace.projects.contains(&path) => {
                if self.workspace.project != path {
                    self.select_project(path, window, cx);
                }
                self.new_browser_pane(Some(url), window, cx);
            }
            Opens::Project(_) | Opens::Here => self.new_browser_pane(Some(url), window, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::opens_in_app;
    use std::net::IpAddr;

    fn local(url: &str) -> bool {
        opens_in_app(url, false, false, &[])
    }

    #[test]
    fn local_addresses_open_in_monocode_and_others_in_the_system_browser() {
        for url in [
            "http://localhost",
            "http://localhost:3000",
            "http://localhost:3000/a/b?c=d#e",
            "https://localhost:8443/",
            "http://app.localhost:5173",
            "http://a.b.localhost",
            "http://printer.local/",
            "http://127.0.0.1:8000",
            "http://127.1.2.3",
            "http://0.0.0.0:8080",
            "http://[::1]:3000/",
            "http://[::]:3000",
            "http://[::ffff:127.0.0.1]:3000",
            "http://localhost.:3000",
            "http://user:pass@localhost:3000",
            "http://localhost?x",
            "http://localhost#x",
        ] {
            assert!(local(url), "{url} should open in Monocode");
        }
        for url in [
            "https://example.com",
            "https://github.com/a/b/pull/1",
            "http://localhost.example.com:3000",
            "http://mylocalhost:3000",
            "http://example.local.com",
            "http://localhost@example.com",
            "http://localhost:3000@example.com/",
            "http://192.168.1.20:3000",
            "http://10.0.0.1",
            "http://[2001:db8::1]:3000",
            "http://8.8.8.8",
            "http://",
        ] {
            assert!(!local(url), "{url} should open in the system browser");
        }
    }

    #[test]
    fn case_does_not_matter() {
        assert!(local("HTTP://LOCALHOST:3000/Path"));
        assert!(local("Https://App.LocalHost"));
        assert!(local("http://Printer.LOCAL"));
        assert!(!local("HTTPS://EXAMPLE.COM"));
        assert!(opens_in_app("HTTPS://EXAMPLE.COM", true, false, &[]));
    }

    #[test]
    fn the_machines_own_addresses_are_local() {
        let machine: Vec<IpAddr> =
            vec!["192.168.1.20".parse().unwrap(), "fe80::1".parse().unwrap()];
        assert!(opens_in_app(
            "http://192.168.1.20:3000",
            false,
            false,
            &machine
        ));
        assert!(opens_in_app(
            "http://[fe80::1]:3000",
            false,
            false,
            &machine
        ));
        assert!(!opens_in_app(
            "http://192.168.1.21:3000",
            false,
            false,
            &machine
        ));
    }

    #[test]
    fn the_setting_sends_every_web_link_to_monocode() {
        assert!(opens_in_app("https://example.com", true, false, &[]));
        assert!(opens_in_app("http://localhost:3000", true, false, &[]));
    }

    #[test]
    fn shift_click_inverts_the_choice() {
        assert!(!opens_in_app("http://localhost:3000", false, true, &[]));
        assert!(opens_in_app("https://example.com", false, true, &[]));
        assert!(!opens_in_app("https://example.com", true, true, &[]));
        assert!(!opens_in_app("http://localhost:3000", true, true, &[]));
    }

    #[test]
    fn links_that_are_not_web_pages_never_open_in_monocode() {
        for url in [
            "mailto:me@localhost",
            "MAILTO:someone@example.com",
            "file:///Users/me/index.html",
            "file://localhost/etc/hosts",
            "ftp://localhost/",
            "ws://localhost:3000",
            "javascript:alert(1)",
            "localhost:3000",
            "/relative/path",
            "",
        ] {
            for (all, invert) in [(false, false), (false, true), (true, false), (true, true)] {
                assert!(
                    !opens_in_app(url, all, invert, &[]),
                    "{url} (all {all}, invert {invert}) should not open in Monocode"
                );
            }
        }
    }

    use super::{LinkTarget, Opens, add_route, link_match, route};

    fn rules() -> Vec<super::LinkRoute> {
        let mut routes = Vec::new();
        let docs = link_match("domain", "https://docs.example.com/a").unwrap();
        add_route(&mut routes, docs, LinkTarget::Little);
        let example = link_match("domain", "example.com").unwrap();
        add_route(&mut routes, example, LinkTarget::Browser);
        let github = link_match("contains", "github.com/acme").unwrap();
        add_route(&mut routes, github, LinkTarget::Project("/w/acme".into()));
        let local = link_match("equal", "localhost").unwrap();
        add_route(&mut routes, local, LinkTarget::System);
        routes
    }

    #[test]
    fn the_first_enabled_rule_that_matches_decides() {
        let routes = rules();
        let at = |url: &str| route(url, &routes, false, false, false, &[]);
        assert_eq!(at("https://docs.example.com/guide"), Opens::Little);
        assert_eq!(at("https://www.example.com/"), Opens::Browser);
        assert_eq!(at("https://example.com/"), Opens::Browser);
        assert_eq!(at("https://badexample.com/"), Opens::System);
        assert_eq!(
            at("https://github.com/acme/app/pull/1"),
            Opens::Project("/w/acme".into())
        );
        assert_eq!(at("http://localhost:5173/"), Opens::System);
        // No rule: K19.
        assert_eq!(at("http://127.0.0.1:8000/"), Opens::Here);
        assert_eq!(at("https://rust-lang.org/"), Opens::System);
        let mut routes = routes;
        routes[0].enabled = false;
        assert_eq!(
            route(
                "https://docs.example.com/",
                &routes,
                false,
                false,
                false,
                &[]
            ),
            Opens::Browser
        );
        // A later rule's id is past every earlier one.
        let id = add_route(
            &mut routes,
            link_match("domain", "a.b").unwrap(),
            LinkTarget::Little,
        );
        assert_eq!(id, 5);
    }

    #[test]
    fn shift_click_and_links_from_other_apps() {
        let routes = rules();
        // ⇧-click: Monocode's targets go to the system browser and back.
        assert_eq!(
            route("https://example.com/", &routes, false, true, false, &[]),
            Opens::System
        );
        assert_eq!(
            route("http://localhost:5173/", &routes, false, true, false, &[]),
            Opens::Here
        );
        // Another app's link opens in the Little window unless a rule says
        // otherwise, and never goes back to the system browser.
        let outside = |url: &str| route(url, &routes, false, false, true, &[]);
        assert_eq!(outside("https://rust-lang.org/"), Opens::Little);
        assert_eq!(outside("https://example.com/"), Opens::Browser);
        assert_eq!(outside("http://localhost:3000/"), Opens::Little);
        // Not a web page: the system's, whatever the rules.
        let everything = vec![super::LinkRoute {
            id: 1,
            enabled: true,
            condition: link_match("contains", ":").unwrap(),
            target: LinkTarget::Browser,
        }];
        assert_eq!(
            route(
                "mailto:me@example.com",
                &everything,
                true,
                false,
                false,
                &[]
            ),
            Opens::System
        );
        assert!(link_match("domain", "  ").is_none());
    }

    use super::queued;

    #[gpui::test]
    fn a_click_queues_a_local_page_and_hands_the_rest_to_the_system(cx: &mut gpui::TestAppContext) {
        let pane = cfg!(target_os = "macos");
        cx.update(|cx| cx.set_global(crate::settings::Preferences::default()));
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            super::open_link("http://localhost:5173/", false, window, cx);
            assert_eq!(
                queued(cx),
                if pane {
                    vec!["http://localhost:5173/".to_owned()]
                } else {
                    Vec::new()
                }
            );
            super::open_link("https://example.com/", false, window, cx);
        });
        assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/"));
        cx.update(|window, cx| {
            // ⇧-click: the local page goes to the system browser instead.
            super::open_link("http://localhost:5173/other", true, window, cx);
            assert_eq!(queued(cx).len(), usize::from(pane));
        });
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("http://localhost:5173/other")
        );
        cx.update(|window, cx| {
            cx.global_mut::<crate::settings::Preferences>()
                .browser_all_links = true;
            super::open_link("https://example.com/docs", false, window, cx);
            assert_eq!(queued(cx).len(), 2 * usize::from(pane));
            super::open_link("mailto:me@example.com", false, window, cx);
        });
        assert_eq!(cx.opened_url().as_deref(), Some("mailto:me@example.com"));
    }

    #[gpui::test]
    fn a_shell_takes_only_its_own_windows_pages_and_unclaimed_ones(cx: &mut gpui::TestAppContext) {
        let first = cx
            .add_empty_window()
            .update(|window, _| window.window_handle());
        let second = cx
            .add_empty_window()
            .update(|window, _| window.window_handle());
        cx.update(|cx| {
            let here = super::Opens::Here;
            cx.set_global(super::LinkQueue(vec![
                (Some(first), "http://localhost:1/".into(), here.clone()),
                (Some(second), "http://localhost:2/".into(), here.clone()),
                (None, "http://localhost:3/".into(), super::Opens::Little),
            ]));
            assert_eq!(
                super::take_for(first, cx),
                [
                    ("http://localhost:1/".to_owned(), here.clone()),
                    ("http://localhost:3/".to_owned(), super::Opens::Little)
                ]
            );
            assert_eq!(queued(cx), ["http://localhost:2/"]);
            assert!(super::take_for(first, cx).is_empty());
            assert_eq!(
                super::take_for(second, cx),
                [("http://localhost:2/".to_owned(), here)]
            );
            assert!(!super::pending(cx));
        });
    }

    #[gpui::test]
    fn a_local_link_opens_as_a_page_in_the_window_it_was_clicked_in(cx: &mut gpui::TestAppContext) {
        if !cfg!(target_os = "macos") {
            return;
        }
        cx.update(|cx| cx.set_reduce_motion(true));
        let (clicked, _, _) = super::super::tests::window(cx);
        let (other, _, _) = super::super::tests::window(cx);
        cx.update(|cx| cx.set_global(crate::settings::Preferences::default()));
        clicked
            .update(cx, |_, window, cx| {
                super::open_link("http://localhost:5173/", false, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        let pages = |window: gpui::WindowHandle<super::Shell>, cx: &mut gpui::TestAppContext| {
            window
                .update(cx, |shell, _, _| {
                    (shell.workspace.browser_panes.values())
                        .map(|pane| pane.url.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap()
        };
        assert_eq!(pages(clicked, cx), ["http://localhost:5173/"]);
        assert!(pages(other, cx).is_empty());
        cx.update(|cx| assert!(!super::pending(cx)));
    }
}
