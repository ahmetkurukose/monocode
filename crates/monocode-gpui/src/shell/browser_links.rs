//! Where a web link clicked in the app opens (K19): a local address, such
//! as a dev server's, in Monocode's browser; anything else in the user's own
//! browser, where they are signed in. "Open all links in Monocode" sends
//! every web link to Monocode, and ⇧-click opens a link in the other one.
//! Links that need the user's real browser session (sign-ins, GitHub's pull
//! request pages) call `cx.open_url` themselves and never come here.
use super::Shell;
use gpui::{AnyWindowHandle, App, Global, Window};
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
fn web_host(url: &str) -> Option<String> {
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

/// `opens_in_app` with the user's setting and this machine's addresses.
/// Only macOS has the browser pane.
fn opens_in_app_now(url: &str, invert: bool, cx: &App) -> bool {
    let all_links = cx
        .try_global::<crate::settings::Preferences>()
        .is_some_and(|preferences| preferences.browser_all_links);
    cfg!(target_os = "macos")
        && opens_in_app(
            url,
            all_links,
            invert,
            &monocode_core::code::run::machine_ips(),
        )
}

/// Pages a click sent to Monocode's browser, waiting for a shell's next
/// frame. Each names the window it was clicked in, so that window's shell
/// opens it; `None` (a floating chat, which has no shell) lets whichever
/// shell draws first take it.
#[derive(Default)]
pub(crate) struct LinkQueue(pub(crate) Vec<(Option<AnyWindowHandle>, String)>);
impl Global for LinkQueue {}

/// Opens a clicked web link where `opens_in_app` says. The one way in for
/// every view (a terminal, a reply), since most have no shell to call: a
/// page for Monocode's browser waits in `LinkQueue` for the shell of the
/// window it was clicked in.
pub(crate) fn open_link(url: &str, invert: bool, window: &Window, cx: &mut App) {
    if opens_in_app_now(url, invert, cx) {
        let owner = (window.root::<Shell>().flatten().is_some()).then(|| window.window_handle());
        (cx.default_global::<LinkQueue>().0).push((owner, url.to_owned()));
        cx.refresh_windows();
    } else {
        cx.open_url(url);
    }
}

/// Takes the queued pages `window`'s shell should open: its own, those no
/// shell's window claimed, and those whose window closed before drawing.
pub(super) fn take_for(window: AnyWindowHandle, cx: &mut App) -> Vec<String> {
    let Some(queue) = cx.try_global::<LinkQueue>() else {
        return Vec::new();
    };
    if queue.0.is_empty() {
        return Vec::new();
    }
    let open = cx.windows();
    let mut taken = Vec::new();
    cx.global_mut::<LinkQueue>().0.retain(|(owner, url)| {
        let ours = owner.is_none_or(|owner| owner == window || !open.contains(&owner));
        if ours {
            taken.push(url.clone());
        }
        !ours
    });
    taken
}

/// The queued pages' addresses, for tests.
#[cfg(test)]
pub(crate) fn queued(cx: &App) -> Vec<String> {
    (cx.try_global::<LinkQueue>()).map_or_else(Vec::new, |queue| {
        queue.0.iter().map(|(_, url)| url.clone()).collect()
    })
}

/// Whether a page waits for some shell, so it should look on its next frame.
pub(super) fn pending(cx: &App) -> bool {
    cx.try_global::<LinkQueue>()
        .is_some_and(|queue| !queue.0.is_empty())
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
            cx.set_global(super::LinkQueue(vec![
                (Some(first), "http://localhost:1/".into()),
                (Some(second), "http://localhost:2/".into()),
                (None, "http://localhost:3/".into()),
            ]));
            assert_eq!(
                super::take_for(first, cx),
                ["http://localhost:1/", "http://localhost:3/"]
            );
            assert_eq!(queued(cx), ["http://localhost:2/"]);
            assert!(super::take_for(first, cx).is_empty());
            assert_eq!(super::take_for(second, cx), ["http://localhost:2/"]);
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
