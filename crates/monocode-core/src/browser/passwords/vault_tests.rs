use super::*;

fn key() -> VaultKey {
    VaultKey::generate().unwrap()
}

#[test]
fn origins_are_scheme_host_and_port_of_web_addresses() {
    assert_eq!(
        origin_of("https://Example.com/login?next=/").as_deref(),
        Some("https://example.com")
    );
    assert_eq!(
        origin_of("http://localhost:3000/a").as_deref(),
        Some("http://localhost:3000")
    );
    assert_eq!(
        origin_of("https://a.test:443/").as_deref(),
        Some("https://a.test")
    );
    assert_eq!(origin_of("about:blank"), None);
    assert_eq!(origin_of("file:///etc/passwd"), None);
    assert_eq!(origin_of("data:text/html,x"), None);
}

#[test]
fn a_saved_password_is_sealed_on_disk_and_opens_with_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default.db");
    let key = key();
    let vault = Vault::open(&path, key.clone()).unwrap();
    let id = vault
        .save("https://a.test/login", "alice", "correct horse", 10)
        .unwrap();
    assert_eq!(vault.password(id).unwrap(), "correct horse");
    let login = vault.get(id).unwrap().unwrap();
    assert_eq!(login.origin, "https://a.test");
    assert_eq!(login.signon_realm, "https://a.test/");
    assert_eq!(
        (login.created_at, login.changed_at, login.times_used),
        (10, 10, 0)
    );
    drop(vault);
    // The file never holds the password in the clear.
    let mut bytes = std::fs::read(&path).unwrap();
    for extra in ["default.db-wal"] {
        if let Ok(more) = std::fs::read(dir.path().join(extra)) {
            bytes.extend(more);
        }
    }
    assert!(!bytes.windows(13).any(|w| w == b"correct horse"));
    // The same key opens it again; another key is refused.
    assert_eq!(
        Vault::open(&path, key).unwrap().password(id).unwrap(),
        "correct horse"
    );
    assert!(Vault::open(&path, VaultKey::generate().unwrap()).is_err());
}

#[test]
fn saving_again_changes_the_password_and_use_is_counted() {
    let vault = Vault::in_memory(key()).unwrap();
    let id = vault.save("https://a.test/", "alice", "one", 1).unwrap();
    assert_eq!(
        vault.save("https://a.test/x", "alice", "two", 5).unwrap(),
        id
    );
    assert_eq!(vault.password(id).unwrap(), "two");
    vault.note_used(id, 7).unwrap();
    vault.note_used(id, 9).unwrap();
    let login = vault.get(id).unwrap().unwrap();
    assert_eq!(
        (login.changed_at, login.last_used_at, login.times_used),
        (5, 9, 2)
    );
    assert!(vault.save("https://a.test/", "bob", "", 1).is_err());
    assert!(vault.save("about:blank", "bob", "x", 1).is_err());
}

#[test]
fn edit_and_delete() {
    let vault = Vault::in_memory(key()).unwrap();
    let a = vault.save("https://a.test/", "alice", "one", 1).unwrap();
    vault.save("https://a.test/", "bob", "two", 1).unwrap();
    vault.edit(a, "alice2", "uno", 3).unwrap();
    assert_eq!(vault.password(a).unwrap(), "uno");
    assert_eq!(vault.get(a).unwrap().unwrap().username, "alice2");
    // Another login's user name is taken.
    assert!(vault.edit(a, "bob", "x", 4).is_err());
    vault.delete(a).unwrap();
    let names: Vec<String> = (vault.logins(None).unwrap().into_iter())
        .map(|l| l.username)
        .collect();
    assert_eq!(names, ["bob"]);
}

#[test]
fn the_offer_follows_chromiums_save_or_update_table() {
    let vault = Vault::in_memory(key()).unwrap();
    let url = "https://a.test/login";
    assert_eq!(vault.offer(url, "alice", "one").unwrap(), Offer::Save);
    let id = vault.save(url, "alice", "one", 1).unwrap();
    // The same: nothing. Another password: update.
    assert_eq!(vault.offer(url, "alice", "one").unwrap(), Offer::Nothing);
    assert_eq!(
        vault.offer(url, "alice", "two").unwrap(),
        Offer::Update {
            id,
            username: "alice".into()
        }
    );
    // Another user: save. A password-only step updates the only login.
    assert_eq!(vault.offer(url, "bob", "x").unwrap(), Offer::Save);
    assert_eq!(
        vault.offer(url, "", "three").unwrap(),
        Offer::Update {
            id,
            username: "alice".into()
        }
    );
    assert_eq!(vault.offer(url, "", "one").unwrap(), Offer::Nothing);
    vault.save(url, "bob", "x", 1).unwrap();
    assert_eq!(vault.offer(url, "", "new").unwrap(), Offer::Save);
    // No password, not a web page, or never for this site: nothing.
    assert_eq!(vault.offer(url, "carol", "").unwrap(), Offer::Nothing);
    assert_eq!(
        vault.offer("about:blank", "c", "x").unwrap(),
        Offer::Nothing
    );
    vault.never_save(url, 2).unwrap();
    assert!(vault.is_never("https://a.test").unwrap());
    assert_eq!(vault.offer(url, "carol", "x").unwrap(), Offer::Nothing);
    assert_eq!(vault.never_list().unwrap(), ["https://a.test"]);
    vault.allow_saving("https://a.test").unwrap();
    assert_eq!(vault.offer(url, "carol", "x").unwrap(), Offer::Save);
}

fn imported(
    origin: &str,
    user: &str,
    password: &str,
    created: i64,
    modified: i64,
) -> ImportedLogin {
    ImportedLogin {
        origin: origin.into(),
        signon_realm: format!("{origin}/"),
        username: user.into(),
        password: password.into(),
        created_ms: created,
        last_used_ms: 0,
        password_modified_ms: modified,
        times_used: 0,
    }
}

#[test]
fn imported_logins_merge_by_site_and_user_and_the_newer_password_wins() {
    let vault = Vault::in_memory(key()).unwrap();
    // Saved here at 1000 (Monocode's own sign-ins: changed then).
    let alice = vault.save("https://a.test/", "alice", "one", 1000).unwrap();
    let bob = vault.save("https://b.test/", "bob", "mine", 1000).unwrap();
    let carol = vault
        .save("https://c.test/", "carol", "same", 1000)
        .unwrap();
    let report = vault
        .import(
            &[
                // Changed later in the other browser: replaces.
                imported("https://A.test/login", "alice", "newer", 10, 2000),
                // Older there: what is saved here stays.
                imported("https://b.test", "bob", "older", 10, 500),
                // The same password: kept, and its use counts.
                ImportedLogin {
                    times_used: 7,
                    last_used_ms: 3000,
                    ..imported("https://c.test", "carol", "same", 10, 10)
                },
                // New site and user: added with the other browser's times.
                imported("https://d.test/", "dan", "fresh", 100, 0),
                // No web origin, no password: left out.
                imported("android://x@com.app/", "c", "three", 1, 1),
                imported("https://e.test/", "e", "", 1, 1),
            ],
            &["https://never.test".into(), "not a site".into()],
            5000,
        )
        .unwrap();
    assert_eq!(
        report,
        ImportReport {
            added: 1,
            updated: 1,
            unchanged: 2,
            skipped: 2,
            never_save: 1,
        }
    );
    assert_eq!(vault.password(alice).unwrap(), "newer");
    assert_eq!(vault.get(alice).unwrap().unwrap().changed_at, 2000);
    assert_eq!(vault.password(bob).unwrap(), "mine");
    let c = vault.get(carol).unwrap().unwrap();
    assert_eq!((c.times_used, c.last_used_at), (7, 3000));
    let dan = &vault.logins(Some("https://d.test")).unwrap()[0];
    assert_eq!(vault.password(dan.id).unwrap(), "fresh");
    // No password change recorded: when it was saved stands in.
    assert_eq!((dan.created_at, dan.changed_at), (100, 100));
    assert!(vault.is_never("https://never.test").unwrap());

    // Importing again merges again: nothing new, nothing replaced.
    let again = vault
        .import(
            &[imported("https://d.test/", "dan", "fresh", 100, 0)],
            &[],
            6000,
        )
        .unwrap();
    assert_eq!((again.added, again.updated, again.unchanged), (0, 0, 1));
    // A tie keeps what is saved.
    let tie = vault
        .import(
            &[imported("https://a.test", "alice", "tied", 10, 2000)],
            &[],
            7000,
        )
        .unwrap();
    assert_eq!(tie.unchanged, 1);
    assert_eq!(vault.password(alice).unwrap(), "newer");
}

#[test]
fn the_vault_and_the_import_merge_agree() {
    // The vault applies `merge_logins` itself: the same logins give the
    // same passwords either way.
    let saved = [
        imported("https://a.test", "me", "old", 100, 100),
        imported("https://b.test", "me", "kept", 500, 500),
    ];
    let incoming = [
        imported("https://a.test", "me", "new", 100, 200),
        imported("https://b.test", "me", "older", 100, 100),
        imported("https://a.test", "you", "x", 1, 0),
    ];
    let vault = Vault::in_memory(key()).unwrap();
    vault.import(&saved, &[], 1).unwrap();
    vault.import(&incoming, &[], 1).unwrap();
    let merged = merge_logins(saved.to_vec(), incoming.to_vec());
    for login in &merged.logins {
        let found = vault.logins(Some(&login.origin)).unwrap();
        let row = found.iter().find(|l| l.username == login.username).unwrap();
        assert_eq!(vault.password(row.id).unwrap(), login.password);
    }
    assert_eq!(vault.logins(None).unwrap().len(), merged.logins.len());
}

#[test]
fn each_profile_has_its_own_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut vaults = Vaults::new(Some(dir.path().to_owned()), key());
    assert!(!Vaults::exist_in(dir.path()));
    vaults
        .vault("default")
        .unwrap()
        .save("https://a.test/", "a", "1", 1)
        .unwrap();
    vaults
        .vault("proj-1")
        .unwrap()
        .save("https://a.test/", "b", "2", 1)
        .unwrap();
    assert!(dir.path().join("default.db").exists());
    assert!(dir.path().join("proj-1.db").exists());
    assert!(Vaults::exist_in(dir.path()));
    assert_eq!(vaults.profiles(), ["default", "proj-1"]);
    assert_eq!(
        vaults.vault("proj-1").unwrap().logins(None).unwrap().len(),
        1
    );
    // Nothing climbs out of the folder.
    vaults.vault("../evil").unwrap();
    assert!(dir.path().join("---evil.db").exists());
}
