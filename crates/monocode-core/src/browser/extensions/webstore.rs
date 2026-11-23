//! The Chrome Web Store's protocol: which extension a store URL names, the
//! package download (`update2/crx`, redirects followed by hand and only to
//! Google's hosts, credentials never sent), and the Omaha 3.1 update check
//! (`update2/json`) that Orca and Chrome use. Blocking; run it off the main
//! thread.

use std::{io::Read, time::Duration};

use serde_json::{Value, json};

/// The Chromium version the store is told: CEF 156's.
pub const CHROME_VERSION: &str = "156.0.0.0";
const UPDATE_URL: &str = "https://update.googleapis.com/service/update2/json";
/// Bigger packages are refused.
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;
const MAX_REDIRECTS: usize = 8;

/// The extension id a store page URL or a bare id names.
pub fn parse_store_input(input: &str) -> Option<String> {
    let input = input.trim();
    if super::is_valid_id(input) {
        return Some(input.to_owned());
    }
    let url = url::Url::parse(input).ok()?;
    let host = url.host_str()?;
    if !matches!(host, "chromewebstore.google.com" | "chrome.google.com") {
        return None;
    }
    // …/detail/<slug>/<id> or …/detail/<id>
    let segments: Vec<&str> = url.path_segments()?.collect();
    let at = segments.iter().position(|s| *s == "detail")?;
    segments[at + 1..]
        .iter()
        .find(|s| super::is_valid_id(s))
        .map(|s| (*s).to_owned())
}

/// The package's download address.
pub fn download_url(id: &str) -> String {
    format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&acceptformat=crx3&prodversion={CHROME_VERSION}&x=id%3D{id}%26installsource%3Dondemand%26uc"
    )
}

/// Packages come only from Google's hosts, over https.
pub fn allowed_download_url(url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    let host = url.host_str().unwrap_or("");
    url.scheme() == "https"
        && [
            "google.com",
            "googleusercontent.com",
            "googleapis.com",
            "gvt1.com",
        ]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .user_agent(&format!("Mozilla/5.0 (Macintosh) Chrome/{CHROME_VERSION}"))
        .build()
}

/// Downloads a package from `url` (redirects followed only to Google).
pub fn download(url: &str, expected_sha256: Option<&str>) -> Result<Vec<u8>, String> {
    let agent = agent();
    let mut url = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        if !allowed_download_url(&url) {
            return Err(format!("refused to download from {url}"));
        }
        let response = match agent.get(&url).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(code, _)) => {
                return Err(format!("the Web Store answered {code}"));
            }
            Err(e) => return Err(e.to_string()),
        };
        if (300..400).contains(&response.status()) {
            let location = response
                .header("location")
                .ok_or("a redirect without a location")?;
            url = url::Url::parse(&url)
                .and_then(|base| base.join(location))
                .map_err(|e| e.to_string())?
                .to_string();
            continue;
        }
        if response.status() == 204 {
            return Err("the Web Store has no package for this extension (it may not exist, or not be available for Chrome)".into());
        }
        let mut body = Vec::new();
        response
            .into_reader()
            .take(MAX_PACKAGE + 1)
            .read_to_end(&mut body)
            .map_err(|e| e.to_string())?;
        if body.len() as u64 > MAX_PACKAGE {
            return Err("the package is too large".into());
        }
        if let Some(want) = expected_sha256 {
            use sha2::Digest;
            let got: String = sha2::Sha256::digest(&body)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if !got.eq_ignore_ascii_case(want) {
                return Err("the package does not match its checksum".into());
            }
        }
        return Ok(body);
    }
    Err("too many redirects".into())
}

/// A newer version the store offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub id: String,
    pub version: String,
    pub url: String,
    pub sha256: Option<String>,
}

/// The update check's request body for `(id, version)` pairs.
pub fn update_request(installed: &[(String, String)], request_id: &str) -> Value {
    json!({
        "request": {
            "@updater": "chromiumcrx",
            "acceptformat": "crx3",
            "app": installed.iter().map(|(id, version)| json!({
                "appid": id,
                "version": version,
                "installsource": "",
                "updatecheck": {},
            })).collect::<Vec<_>>(),
            "os": {"platform": "mac", "arch": std::env::consts::ARCH},
            "prodversion": CHROME_VERSION,
            "protocol": "3.1",
            "requestid": format!("{{{request_id}}}"),
            "sessionid": format!("{{{request_id}}}"),
        }
    })
}

/// The updates in an Omaha response newer than what is installed; anything
/// malformed or offered from elsewhere than Google is skipped.
pub fn parse_update_response(text: &str, installed: &[(String, String)]) -> Vec<Update> {
    let body = text.strip_prefix(")]}'").unwrap_or(text);
    let Ok(parsed) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for app in parsed["response"]["app"].as_array().into_iter().flatten() {
        let check = &app["updatecheck"];
        let (Some(id), Some("ok")) = (app["appid"].as_str(), check["status"].as_str()) else {
            continue;
        };
        let Some((_, current)) = installed.iter().find(|(i, _)| i == id) else {
            continue;
        };
        let Some(version) = check["manifest"]["version"].as_str() else {
            continue;
        };
        if super::compare_versions(version, current).is_le() {
            continue;
        }
        let package = &check["manifest"]["packages"]["package"][0];
        let (Some(name), Some(codebase)) = (
            package["name"].as_str(),
            check["urls"]["url"][0]["codebase"].as_str(),
        ) else {
            continue;
        };
        let Ok(url) = url::Url::parse(codebase).and_then(|b| b.join(name)) else {
            continue;
        };
        let url = url.to_string();
        if !allowed_download_url(&url) {
            continue;
        }
        out.push(Update {
            id: id.to_owned(),
            version: version.to_owned(),
            url,
            sha256: package["hash_sha256"].as_str().map(str::to_owned),
        });
    }
    out
}

/// Asks the store which of `installed` have updates.
pub fn check_updates(installed: &[(String, String)]) -> Result<Vec<Update>, String> {
    if installed.is_empty() {
        return Ok(Vec::new());
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let ids: Vec<&str> = installed.iter().map(|(id, _)| id.as_str()).collect();
    let response = agent()
        .post(UPDATE_URL)
        .set("Content-Type", "application/json")
        .set("X-Goog-Update-Interactivity", "bg")
        .set("X-Goog-Update-AppId", &ids.join(","))
        .set(
            "X-Goog-Update-Updater",
            &format!("chromiumcrx-{CHROME_VERSION}"),
        )
        .send_string(&update_request(installed, &request_id).to_string())
        .map_err(|e| e.to_string())?;
    let text = response.into_string().map_err(|e| e.to_string())?;
    Ok(parse_update_response(&text, installed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::extensions::PROTON_PASS_ID;

    #[test]
    fn store_urls_and_ids() {
        assert_eq!(
            parse_store_input(PROTON_PASS_ID).as_deref(),
            Some(PROTON_PASS_ID)
        );
        let url = format!(
            "https://chromewebstore.google.com/detail/proton-pass-free-password/{PROTON_PASS_ID}?hl=en"
        );
        assert_eq!(parse_store_input(&url).as_deref(), Some(PROTON_PASS_ID));
        let old = format!("https://chrome.google.com/webstore/detail/{PROTON_PASS_ID}");
        assert_eq!(parse_store_input(&old).as_deref(), Some(PROTON_PASS_ID));
        assert_eq!(
            parse_store_input("https://evil.com/detail/x/ghmbeldphafepmbegfdlkpapadhbakde"),
            None
        );
        assert_eq!(parse_store_input("hello"), None);
        assert!(download_url(PROTON_PASS_ID).contains("id%3Dghmbeld"));
    }

    #[test]
    fn downloads_only_from_google() {
        assert!(allowed_download_url(
            "https://clients2.googleusercontent.com/crx/blobs/x.crx"
        ));
        assert!(allowed_download_url("https://edgedl.me.gvt1.com/x.crx"));
        assert!(!allowed_download_url("http://clients2.google.com/x"));
        assert!(!allowed_download_url("https://google.com.evil.com/x"));
        assert!(!allowed_download_url("https://evilgoogle.com/x"));
    }

    #[test]
    fn update_responses() {
        let installed = vec![
            (PROTON_PASS_ID.to_owned(), "1.44.0".to_owned()),
            ("b".repeat(32), "2.0".to_owned()),
        ];
        let text = format!(
            ")]}}'{}",
            json!({"response": {"app": [
                {"appid": PROTON_PASS_ID, "updatecheck": {"status": "ok",
                    "urls": {"url": [{"codebase": "https://edgedl.me.gvt1.com/edgedl/release2/"}]},
                    "manifest": {"version": "1.45.0", "packages": {"package": [{"name": "pass_1_45_0.crx", "hash_sha256": "ab"}]}}}},
                {"appid": "b".repeat(32), "updatecheck": {"status": "ok",
                    "urls": {"url": [{"codebase": "https://evil.com/"}]},
                    "manifest": {"version": "3.0", "packages": {"package": [{"name": "x.crx"}]}}}},
                {"appid": "c".repeat(32), "updatecheck": {"status": "noupdate"}}
            ]}})
        );
        let updates = parse_update_response(&text, &installed);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].version, "1.45.0");
        assert_eq!(
            updates[0].url,
            "https://edgedl.me.gvt1.com/edgedl/release2/pass_1_45_0.crx"
        );
        assert_eq!(updates[0].sha256.as_deref(), Some("ab"));
        // Not newer: nothing.
        let same = vec![(PROTON_PASS_ID.to_owned(), "1.45.0".to_owned())];
        assert!(parse_update_response(&text, &same).is_empty());
        assert!(parse_update_response("garbage", &installed).is_empty());
        let body = update_request(&installed, "x");
        assert_eq!(body["request"]["app"][0]["appid"], PROTON_PASS_ID);
        assert_eq!(body["request"]["acceptformat"], "crx3");
    }
}
