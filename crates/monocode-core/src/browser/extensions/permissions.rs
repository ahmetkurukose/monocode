//! Install warnings after Chromium's permission messages (Orca's
//! `browser-extension-permission-warnings.ts`): what the user is told an
//! extension may do, and whether an update asks for more, in which case it
//! waits for the user (as Chrome disables such an extension until then).

use super::pattern::MatchPattern;

/// One warning, as Chrome words it.
pub fn warning(permission: &str) -> Option<&'static str> {
    Some(match permission {
        "tabs" | "webNavigation" => "Read your browsing history",
        "history" => "Read and change your browsing history",
        "bookmarks" => "Read and change your bookmarks",
        "clipboardRead" => "Read data you copy and paste",
        "clipboardWrite" => "Modify data you copy and paste",
        "downloads" => "Manage your downloads",
        "nativeMessaging" => "Communicate with cooperating native applications",
        "notifications" => "Display notifications",
        "privacy" => "Change your privacy-related settings",
        "management" => "Manage your apps, extensions, and themes",
        "proxy" => "Read and change all your data on all websites",
        "contentSettings" => "Change your settings that control websites' access to features",
        "geolocation" => "Detect your physical location",
        "topSites" => "Read a list of your most frequently visited websites",
        "declarativeNetRequest" => "Block content on any page",
        "debugger" => "Access the page debugger backend",
        "desktopCapture" | "tabCapture" => "Capture content of your screen",
        _ => return None,
    })
}

/// The host a host permission names (`*://*.example.com/*` → example.com);
/// `None` when it covers every site.
pub fn host_label(pattern: &str) -> Option<String> {
    let parsed = MatchPattern::parse(pattern)?;
    if parsed.covers_all_hosts() {
        return None;
    }
    let host = pattern.split_once("://")?.1.split('/').next()?;
    let host = host.split(':').next().unwrap_or(host);
    Some(host.trim_start_matches("*.").to_owned())
}

/// The warnings an extension with these permissions and hosts shows, in
/// Chrome's order: sites first.
pub fn warnings(permissions: &[String], hosts: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if !hosts.is_empty() {
        let labels: Vec<Option<String>> = hosts.iter().map(|h| host_label(h)).collect();
        if labels.iter().any(Option::is_none) {
            out.push("Read and change all your data on all websites".to_owned());
        } else {
            let mut labels: Vec<String> = labels.into_iter().flatten().collect();
            labels.sort();
            labels.dedup();
            out.push(match labels.len() {
                1 => format!("Read and change your data on {}", labels[0]),
                _ => format!("Read and change your data on {}", labels.join(", ")),
            });
        }
    }
    let mut seen = Vec::new();
    for p in permissions {
        if let Some(w) = warning(p) {
            if !seen.contains(&w) {
                seen.push(w);
                out.push(w.to_owned());
            }
        }
    }
    // "Read and change history" covers "read history".
    if seen.contains(&"Read and change your browsing history") {
        out.retain(|w| w != "Read your browsing history");
    }
    out
}

/// The warnings of `new` that `granted` does not show yet: an update with
/// any waits for the user.
pub fn added_warnings(new: (&[String], &[String]), granted: (&[String], &[String])) -> Vec<String> {
    let before = warnings(granted.0, granted.1);
    let all_sites = before.iter().any(|w| w.ends_with("on all websites"));
    let granted_hosts: Vec<Option<String>> = granted.1.iter().map(|h| host_label(h)).collect();
    let mut out: Vec<String> = Vec::new();
    for w in warnings(new.0, &[]) {
        let covered = before.contains(&w)
            || (w == "Read your browsing history"
                && before
                    .iter()
                    .any(|b| b == "Read and change your browsing history"));
        if !covered {
            out.push(w);
        }
    }
    if !all_sites {
        let more: Vec<String> = (new.1.iter())
            .filter(|h| {
                let label = host_label(h);
                label.is_none() || !granted_hosts.contains(&label)
            })
            .cloned()
            .collect();
        if !more.is_empty() {
            out.splice(0..0, warnings(&[], &more));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn proton_pass_warns_like_chrome() {
        let perms = v(&[
            "activeTab",
            "alarms",
            "offscreen",
            "scripting",
            "storage",
            "unlimitedStorage",
            "webNavigation",
            "webRequest",
        ]);
        let hosts = v(&["https://*/*", "http://*/*"]);
        assert_eq!(
            warnings(&perms, &hosts),
            v(&[
                "Read and change all your data on all websites",
                "Read your browsing history"
            ])
        );
    }

    #[test]
    fn updates_that_add_warnings_wait() {
        let granted = (v(&["storage", "tabs"]), v(&["https://a.com/*"]));
        let same = (
            v(&["storage", "webNavigation", "alarms"]),
            v(&["https://*.a.com/*"]),
        );
        assert!(added_warnings((&same.0, &same.1), (&granted.0, &granted.1)).is_empty());
        let more = (v(&["tabs", "clipboardRead"]), v(&["https://b.com/*"]));
        assert_eq!(
            added_warnings((&more.0, &more.1), (&granted.0, &granted.1)),
            v(&[
                "Read and change your data on b.com",
                "Read data you copy and paste"
            ])
        );
        let all = (v(&[]), v(&["<all_urls>"]));
        assert_eq!(
            added_warnings((&all.0, &all.1), (&granted.0, &granted.1)),
            v(&["Read and change all your data on all websites"])
        );
        let granted_all = (v(&[]), v(&["*://*/*"]));
        assert!(
            added_warnings((&more.0[..1], &more.1), (&granted_all.0, &granted_all.1)).len() == 1
        );
    }

    #[test]
    fn host_labels() {
        assert_eq!(
            host_label("*://*.example.com/*").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            host_label("http://localhost:3000/*").as_deref(),
            Some("localhost")
        );
        assert_eq!(host_label("<all_urls>"), None);
        assert_eq!(host_label("https://*/*"), None);
    }
}
