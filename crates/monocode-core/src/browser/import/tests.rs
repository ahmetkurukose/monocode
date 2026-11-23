use super::cookies::{is_non_transplantable, read_cookies};
use super::crypto::{decrypt, strip_host_hash, Decrypted};
use super::fixtures::*;
use super::logins::read_logins;
use super::*;

fn login(origin: &str, user: &str, password: &str, created: i64, modified: i64) -> ImportedLogin {
    ImportedLogin {
        origin: origin.into(),
        signon_realm: format!("{origin}/"),
        username: user.into(),
        password: password.into(),
        created_ms: created,
        last_used_ms: 0,
        password_modified_ms: modified,
        times_used: 1,
    }
}

fn cookie(name: &str, host: &str, path: &str, value: &str) -> ImportedCookie {
    ImportedCookie {
        name: name.into(),
        value: value.into(),
        host_key: host.into(),
        path: path.into(),
        secure: true,
        http_only: false,
        same_site: SameSite::Lax,
        expires: None,
        creation: 0,
        last_access: 0,
    }
}

// ---- discovery ------------------------------------------------------

#[test]
fn discovery_lists_each_browsers_profiles_from_local_state() {
    let home = tempfile::tempdir().unwrap();
    let chrome = SourceBrowser::Chrome.user_data_dir(home.path());
    write_local_state(
        &chrome,
        &[
            ("Default", "Person 1"),
            ("Profile 2", "Work"),
            ("Gone", "Deleted"),
            ("../x", "Evil"),
        ],
    );
    std::fs::create_dir_all(chrome.join("Default/Network")).unwrap();
    std::fs::write(chrome.join("Default/Network/Cookies"), b"").unwrap();
    std::fs::create_dir_all(chrome.join("Profile 2")).unwrap();
    std::fs::write(chrome.join("Profile 2/Login Data"), b"").unwrap();
    // Arc keeps Chromium's layout under "User Data"; no Local State here.
    let arc = home
        .path()
        .join("Library/Application Support/Arc/User Data");
    std::fs::create_dir_all(arc.join("Default")).unwrap();
    std::fs::write(arc.join("Default/Cookies"), b"").unwrap();

    let found = discover(home.path());
    assert_eq!(
        found.iter().map(|b| b.browser).collect::<Vec<_>>(),
        [SourceBrowser::Chrome, SourceBrowser::Arc],
        "Brave is not installed"
    );
    let chrome = &found[0];
    let names: Vec<_> = chrome
        .profiles
        .iter()
        .map(|p| (p.folder.as_str(), p.name.as_str()))
        .collect();
    assert_eq!(names, [("Default", "Person 1"), ("Profile 2", "Work")]);
    assert!(chrome.profiles[0].has_cookies && !chrome.profiles[0].has_logins);
    assert!(!chrome.profiles[1].has_cookies && chrome.profiles[1].has_logins);
    assert_eq!(found[1].profiles[0].name, "Default");
    assert!(
        found[1].profiles[0].has_cookies,
        "the legacy Cookies path counts"
    );
}

#[test]
fn every_browser_has_its_keychain_item_and_folder() {
    let home = std::path::Path::new("/Users/me");
    assert_eq!(
        SourceBrowser::Chrome.keychain_item(),
        ("Chrome Safe Storage", "Chrome")
    );
    assert_eq!(
        SourceBrowser::Arc.keychain_item(),
        ("Arc Safe Storage", "Arc")
    );
    assert_eq!(
        SourceBrowser::Brave.keychain_item(),
        ("Brave Safe Storage", "Brave")
    );
    assert!(SourceBrowser::Brave
        .user_data_dir(home)
        .ends_with("Library/Application Support/BraveSoftware/Brave-Browser"));
    assert!(SourceBrowser::Chrome
        .user_data_dir(home)
        .ends_with("Library/Application Support/Google/Chrome"));
}

// ---- crypto ---------------------------------------------------------

#[test]
fn the_key_is_chromiums_pbkdf2_of_the_password() {
    // Known answer, computed with Python's hashlib and OpenSSL:
    // PBKDF2-HMAC-SHA1("peanuts", "saltysalt", 1003, 16) is
    // d9a09d499b4e1b7461f28e67972c6dbd, and AES-128-CBC of "hello" under it
    // with an IV of 16 spaces is 8690a7cb3c7871d1a16024469ce35b43.
    let key = SafeStorageKey::derive(b"peanuts");
    let mut encrypted = b"v10".to_vec();
    encrypted.extend([
        0x86, 0x90, 0xa7, 0xcb, 0x3c, 0x78, 0x71, 0xd1, 0xa1, 0x60, 0x24, 0x46, 0x9c, 0xe3, 0x5b,
        0x43,
    ]);
    assert_eq!(key.encrypt_v10(b"hello"), encrypted);
    assert_eq!(
        decrypt(&encrypted, &key),
        Decrypted::Plain(b"hello".to_vec())
    );
    assert_ne!(
        decrypt(&encrypted, &SafeStorageKey::derive(b"other")),
        Decrypted::Plain(b"hello".to_vec())
    );
}

#[test]
fn v20_is_app_bound_and_unknown_prefixes_fail() {
    let key = key();
    assert_eq!(decrypt(b"v20whatever-bytes", &key), Decrypted::AppBound);
    assert_eq!(decrypt(b"v11abc", &key), Decrypted::Failed);
    assert_eq!(decrypt(b"v10short", &key), Decrypted::Failed);
    assert_eq!(decrypt(b"", &key), Decrypted::Failed);
}

#[test]
fn the_host_hash_prefix_must_match_the_host() {
    use sha2::Digest;
    let mut plain = sha2::Sha256::digest(b".example.com").to_vec();
    plain.extend(b"value");
    assert_eq!(
        strip_host_hash(plain.clone(), ".example.com"),
        Some(b"value".to_vec())
    );
    assert_eq!(strip_host_hash(plain, ".other.com"), None);
    // An empty value is the hash alone.
    let only = sha2::Sha256::digest(b"a.com").to_vec();
    assert_eq!(strip_host_hash(only, "a.com"), Some(Vec::new()));
}

// ---- time -----------------------------------------------------------

#[test]
fn chromium_time_converts_both_ways() {
    // 1970-01-01 is 11644473600 s after 1601-01-01.
    assert_eq!(chrome_time_to_unix_ms(11_644_473_600_000_000), 0);
    assert_eq!(
        chrome_time_to_unix_ms(13_000_000_000_000_000),
        1_355_526_400_000
    );
    assert_eq!(chrome_time_to_unix_ms(0), 0, "never stays never");
    assert_eq!(chrome_time_to_unix_ms(5), 0, "before 1970 clamps");
    assert_eq!(
        unix_ms_to_chrome_time(1_355_526_400_000),
        13_000_000_000_000_000
    );
    let now = chrome_now();
    assert!(now > 13_400_000_000_000_000, "{now}");
}

// ---- cookies --------------------------------------------------------

#[test]
fn cookies_map_with_their_attributes_at_both_database_versions() {
    for version in [23, 24] {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("Cookies");
        write_cookies(
            &db,
            version,
            &[
                CookieRow::default(),
                CookieRow {
                    host_key: "app.example.org",
                    name: "token",
                    value: "t=1;x",
                    path: "/api",
                    expires: 0,
                    secure: false,
                    http_only: false,
                    same_site: -1,
                    ..Default::default()
                },
                CookieRow {
                    host_key: "strict.example",
                    same_site: 2,
                    value: "",
                    ..Default::default()
                },
            ],
        );
        let read = read_cookies(&db, &key(), now()).unwrap();
        assert_eq!(read.failed, 0, "version {version}");
        assert_eq!(read.cookies.len(), 3);
        let sid = &read.cookies[0];
        assert_eq!((sid.name.as_str(), sid.value.as_str()), ("sid", "abc"));
        assert!(!sid.host_only() && sid.secure && sid.http_only);
        assert_eq!(sid.same_site, SameSite::Lax);
        assert_eq!(sid.expires, Some(now() + 30 * DAY));
        assert_eq!(sid.url(), "https://example.com/");
        let token = &read.cookies[1];
        assert_eq!(token.value, "t=1;x");
        assert!(token.host_only());
        assert_eq!(token.expires, None, "a session cookie");
        assert_eq!(token.same_site, SameSite::Unspecified);
        assert_eq!(token.url(), "http://app.example.org/api");
        assert_eq!(read.cookies[2].same_site, SameSite::Strict);
        assert_eq!(read.cookies[2].value, "");
    }
}

#[test]
fn a_version_24_value_without_its_host_hash_fails() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("Cookies");
    write_cookies(
        &db,
        24,
        &[CookieRow {
            // Encrypted the pre-24 way: no hash prefix.
            encrypted: Some(key().encrypt_v10(b"abcdefghijklmnopqrstuvwxyz0123456789")),
            ..Default::default()
        }],
    );
    let read = read_cookies(&db, &key(), now()).unwrap();
    assert!(read.cookies.is_empty());
    assert_eq!(read.failed, 1);
}

#[test]
fn expired_app_bound_google_and_partitioned_cookies_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("Cookies");
    write_cookies(
        &db,
        24,
        &[
            CookieRow::default(),
            CookieRow {
                expires: now() - 1,
                ..Default::default()
            },
            CookieRow {
                encrypted: Some(b"v20sealed-to-the-browser".to_vec()),
                ..Default::default()
            },
            CookieRow {
                host_key: ".google.com",
                ..Default::default()
            },
            CookieRow {
                host_key: "accounts.google.co.uk",
                ..Default::default()
            },
            CookieRow {
                host_key: ".youtube.com",
                ..Default::default()
            },
            CookieRow {
                partition: "https://top.example",
                ..Default::default()
            },
            CookieRow {
                encrypted: Some(SafeStorageKey::derive(b"wrong").encrypt_v10(b"x")),
                ..Default::default()
            },
        ],
    );
    let read = read_cookies(&db, &key(), now()).unwrap();
    assert_eq!(read.cookies.len(), 1);
    assert_eq!(read.skipped_expired, 1);
    assert_eq!(read.skipped_app_bound, 1);
    assert_eq!(read.skipped_google, 3);
    assert_eq!(read.skipped_unsupported, 1);
    assert_eq!(read.failed, 1);
}

#[test]
fn google_domains_are_a_fixed_list() {
    for host in [
        ".google.com",
        "mail.google.com",
        "google.com",
        ".youtube.com",
        "accounts.google.de",
    ] {
        assert!(is_non_transplantable(host), "{host}");
    }
    for host in [
        "notgoogle.com",
        ".example.com",
        "google.com.evil.example",
        "youtube.co",
    ] {
        assert!(!is_non_transplantable(host), "{host}");
    }
}

// ---- logins ---------------------------------------------------------

#[test]
fn logins_decrypt_and_never_save_sites_come_apart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("Login Data");
    write_logins(
        &db,
        &[
            LoginRow::default(),
            LoginRow {
                origin: "https://blocked.example/",
                never_save: true,
                ..Default::default()
            },
            LoginRow {
                origin: "https://bound.example/",
                encrypted: Some(b"v20sealed".to_vec()),
                ..Default::default()
            },
            LoginRow {
                origin: "https://basic.example/",
                scheme: 1,
                ..Default::default()
            },
        ],
    );
    let read = read_logins(&db, &key()).unwrap();
    assert_eq!(read.logins.len(), 1);
    let l = &read.logins[0];
    assert_eq!(l.origin, "https://example.com");
    assert_eq!(l.signon_realm, "https://example.com/");
    assert_eq!(
        (l.username.as_str(), l.password.as_str()),
        ("me@example.com", "hunter2")
    );
    assert_eq!(l.created_ms, 1_700_000_000_000);
    assert_eq!(l.last_used_ms, 1_700_000_001_000);
    assert_eq!(l.password_modified_ms, 1_700_000_000_000);
    assert_eq!(l.times_used, 3);
    assert_eq!(read.never_save, ["https://blocked.example"]);
    assert_eq!(read.skipped_app_bound, 1);
    assert_eq!(read.skipped_unsupported, 1);
    assert!(
        !format!("{l:?}").contains("hunter2"),
        "Debug hides the password"
    );
}

#[test]
fn read_profile_copies_reads_both_and_asks_the_keychain_once() {
    let home = tempfile::tempdir().unwrap();
    let root = SourceBrowser::Brave.user_data_dir(home.path());
    write_local_state(&root, &[("Default", "Me")]);
    write_cookies(
        &root.join("Default/Network/Cookies"),
        24,
        &[CookieRow::default()],
    );
    write_logins(&root.join("Default/Login Data"), &[LoginRow::default()]);
    write_logins(
        &root.join("Default/Login Data For Account"),
        &[LoginRow {
            password: "newer",
            modified_ms: 1_800_000_000_000,
            ..Default::default()
        }],
    );
    let profile = discover(home.path()).remove(0).profiles.remove(0);
    let storage = FakeStorage::ok();
    let request = ImportRequest {
        profile,
        cookies: true,
        passwords: true,
    };
    let data = read_profile_at(&request, &storage, now()).unwrap();
    assert_eq!(storage.times_asked(), 1);
    assert_eq!(data.cookies.as_ref().unwrap().cookies.len(), 1);
    let logins = data.logins.as_ref().unwrap();
    assert_eq!(logins.logins.len(), 1, "the account copy merges in");
    assert_eq!(logins.logins[0].password, "newer");
    let summary = ImportSummary::of_read(&data);
    assert_eq!(summary.logins_read, 1);
    assert!(summary.lines()[1].starts_with("Passwords: 1 read"));

    let only_cookies = ImportRequest {
        passwords: false,
        ..request.clone()
    };
    let data = read_profile_at(&only_cookies, &storage, now()).unwrap();
    assert!(data.logins.is_none());
}

#[test]
fn read_profile_reports_keychain_and_missing_data_plainly() {
    let home = tempfile::tempdir().unwrap();
    let root = SourceBrowser::Chrome.user_data_dir(home.path());
    write_cookies(&root.join("Default/Network/Cookies"), 24, &[]);
    let profile = discover(home.path()).remove(0).profiles.remove(0);
    let request = ImportRequest {
        profile: profile.clone(),
        cookies: true,
        passwords: false,
    };
    let denied = FakeStorage::failing(KeychainError::Denied);
    let error = read_profile_at(&request, &denied, now()).unwrap_err();
    assert_eq!(error, ImportError::KeychainDenied(SourceBrowser::Chrome));
    assert!(error.to_string().contains("\"Chrome Safe Storage\""));
    let missing = FakeStorage::failing(KeychainError::NotFound);
    assert_eq!(
        read_profile_at(&request, &missing, now()).unwrap_err(),
        ImportError::KeychainMissing(SourceBrowser::Chrome)
    );
    // Passwords only, and the profile has none: nothing to ask the Keychain for.
    let storage = FakeStorage::ok();
    let passwords = ImportRequest {
        cookies: false,
        passwords: true,
        ..request.clone()
    };
    assert!(matches!(
        read_profile_at(&passwords, &storage, now()),
        Err(ImportError::ProfileMissing(_))
    ));
    assert_eq!(storage.times_asked(), 0);
    // A database that is not one.
    std::fs::write(root.join("Default/Network/Cookies"), b"not sqlite at all").unwrap();
    assert!(matches!(
        read_profile_at(&request, &storage, now()),
        Err(ImportError::Unreadable(_))
    ));
}

// ---- merge ----------------------------------------------------------

#[test]
fn a_reimport_overwrites_cookies_of_the_same_name_domain_and_path() {
    let existing = vec![
        cookie("a", ".x.com", "/", "old"),
        cookie("a", ".x.com", "/sub", "other path"),
    ];
    let merged = merge_cookies(
        existing,
        [
            cookie("a", ".X.com", "/", "new"),
            cookie("b", ".x.com", "/", "added"),
        ],
    );
    let values: Vec<_> = merged.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(values, ["new", "other path", "added"]);
}

#[test]
fn logins_merge_by_origin_and_username_and_the_newer_password_wins() {
    let existing = vec![
        login("https://a.com", "me", "old", 100, 100),
        login("https://b.com", "me", "same", 100, 0),
        login("https://c.com", "me", "kept", 500, 500),
    ];
    let merge = merge_logins(
        existing,
        [
            // Newer: replaces.
            login("https://A.com/login", "me", "new", 100, 200),
            // The same password: unchanged, but its use counts.
            ImportedLogin {
                times_used: 9,
                last_used_ms: 999,
                ..login("https://b.com", "me", "same", 300, 300)
            },
            // Older: the one there stays.
            login("https://c.com", "me", "older", 100, 100),
            // Another user: added.
            login("https://a.com", "you", "x", 1, 0),
        ],
    );
    assert_eq!((merge.added, merge.updated, merge.unchanged), (1, 1, 2));
    let by = |origin: &str, user: &str| {
        merge
            .logins
            .iter()
            .find(|l| merge::login_key(l) == (origin.to_owned(), user.to_owned()))
            .unwrap()
            .clone()
    };
    assert_eq!(by("https://a.com", "me").password, "new");
    assert_eq!(by("https://b.com", "me").times_used, 9);
    assert_eq!(by("https://b.com", "me").last_used_ms, 999);
    assert_eq!(by("https://c.com", "me").password, "kept");
    assert_eq!(merge.logins.len(), 4);
}

#[test]
fn a_tie_keeps_what_is_there_and_created_stands_in_for_modified() {
    let merge = merge_logins(
        vec![login("https://a.com", "me", "first", 100, 0)],
        [login("https://a.com", "me", "second", 100, 0)],
    );
    assert_eq!(merge.logins[0].password, "first");
    let merge = merge_logins(
        vec![login("https://a.com", "me", "first", 100, 0)],
        [login("https://a.com", "me", "second", 150, 0)],
    );
    assert_eq!(merge.logins[0].password, "second");
}

#[test]
fn the_summary_says_what_was_skipped_and_why() {
    let summary = ImportSummary {
        cookies_asked: true,
        cookies_imported: 10,
        cookies_google: 3,
        cookies_expired: 2,
        cookies_failed: 1,
        ..Default::default()
    };
    assert_eq!(
        summary.lines(),
        ["Cookies: 10 imported, 5 skipped (3 Google, 2 expired), 1 failed"]
    );
}
