use super::*;

const DAY: i64 = 86_400_000;
const NOW: i64 = 1_800_000_000_000;

fn store() -> Store {
    Store::in_memory(NOW).unwrap()
}

#[test]
fn only_web_pages_are_kept() {
    for url in [
        "https://example.com/",
        "http://localhost:3000/a?b#c",
        "http://[::1]:8000/",
    ] {
        assert!(recordable(url), "{url}");
    }
    for url in [
        "",
        "about:blank",
        "data:text/html,hi",
        "blob:https://example.com/1234",
        "devtools://devtools/bundled/devtools_app.html",
        "chrome://settings",
        "file:///etc/hosts",
        "javascript:alert(1)",
        "not a url",
    ] {
        assert!(!recordable(url), "{url}");
    }
    let store = store();
    assert_eq!(
        store
            .record("/p", "about:blank", "", Transition::Typed, NOW)
            .unwrap(),
        None
    );
    assert!(
        store
            .visits("default", &Query::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn visits_count_per_project_and_keep_title_favicon_and_transition() {
    let store = store();
    let first = store
        .record("/p", "https://a.test/", "", Transition::Typed, NOW - 2_000)
        .unwrap()
        .unwrap();
    store.set_title("/p", "https://a.test/", "A").unwrap();
    store
        .set_favicon("/p", "https://a.test/", "https://a.test/favicon.ico")
        .unwrap();
    store
        .record("/p", "https://a.test/", "", Transition::Link, NOW - 1_000)
        .unwrap();
    // The same page in another project is that project's.
    store
        .record("/q", "https://a.test/", "A in q", Transition::Link, NOW)
        .unwrap();

    let p = store
        .visits(
            "default",
            &Query {
                project: Some("/p".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].visited_at, NOW - 1_000);
    assert_eq!(p[0].title, "A");
    assert_eq!(p[0].favicon.as_deref(), Some("https://a.test/favicon.ico"));
    assert_eq!(p[0].visit_count, 2);
    assert_eq!(p[1].id, first);
    assert_eq!(p[1].transition, Transition::Typed);

    let entries = store.entries(Some("/p"), "", Order::Recent, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].visit_count, entries[0].typed_count), (2, 1));
    let all = store.entries(None, "", Order::Recent, 10).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].project, "/q");
    assert_eq!(all[0].title, "A in q");

    // A visit learned to be an agent's is no longer typed.
    store.set_transition(first, Transition::Agent).unwrap();
    let entries = store.entries(Some("/p"), "", Order::Recent, 10).unwrap();
    assert_eq!(entries[0].typed_count, 0);
}

#[test]
fn search_matches_every_word_in_url_or_title_any_case() {
    let store = store();
    store
        .record(
            "/p",
            "https://docs.rs/serde",
            "Serde docs",
            Transition::Link,
            NOW,
        )
        .unwrap();
    store
        .record(
            "/p",
            "https://example.com/100%_off",
            "Sale",
            Transition::Link,
            NOW,
        )
        .unwrap();
    let find = |text: &str| {
        store
            .visits(
                "default",
                &Query {
                    text: text.into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .into_iter()
            .map(|v| v.url)
            .collect::<Vec<_>>()
    };
    assert_eq!(find("SERDE docs.rs"), ["https://docs.rs/serde"]);
    assert_eq!(find("serde sale"), Vec::<String>::new());
    // LIKE's wildcards are plain characters here.
    assert_eq!(find("100%_"), ["https://example.com/100%_off"]);
    assert_eq!(find("1%0"), Vec::<String>::new());
    assert_eq!(find("").len(), 2);
}

#[test]
fn time_bounds_and_limit_apply() {
    let store = store();
    for (i, at) in [NOW - 3 * DAY, NOW - 2 * DAY, NOW - DAY, NOW]
        .iter()
        .enumerate()
    {
        store
            .record(
                "/p",
                &format!("https://{i}.test/"),
                "",
                Transition::Link,
                *at,
            )
            .unwrap();
    }
    let query = |since, until, limit| {
        store
            .visits(
                "default",
                &Query {
                    since,
                    until,
                    limit,
                    ..Default::default()
                },
            )
            .unwrap()
            .len()
    };
    assert_eq!(query(Some(NOW - 2 * DAY), None, 0), 3);
    assert_eq!(query(None, Some(NOW - DAY), 0), 2);
    assert_eq!(query(None, None, 1), 1);
}

#[test]
fn deleting_a_visit_a_range_a_url_or_everything() {
    let store = store();
    let a1 = store
        .record(
            "/p",
            "https://a.test/",
            "",
            Transition::Typed,
            NOW - 3 * DAY,
        )
        .unwrap()
        .unwrap();
    store
        .record("/p", "https://a.test/", "", Transition::Link, NOW - DAY)
        .unwrap();
    store
        .record("/p", "https://b.test/", "", Transition::Link, NOW)
        .unwrap();
    store
        .record("/q", "https://c.test/", "", Transition::Link, NOW)
        .unwrap();

    store.delete_visit(a1).unwrap();
    let a = &store
        .entries(Some("/p"), "a.test", Order::Recent, 10)
        .unwrap()[0];
    assert_eq!(
        (a.visit_count, a.typed_count, a.last_visit),
        (1, 0, NOW - DAY)
    );

    // The last day of /p: b's visit; a's goes with its last visit.
    store.delete_range(Some("/p"), NOW - DAY, NOW + 1).unwrap();
    assert!(
        store
            .entries(Some("/p"), "", Order::Recent, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .entries(Some("/q"), "", Order::Recent, 10)
            .unwrap()
            .len(),
        1
    );

    store
        .record("/p", "https://a.test/", "", Transition::Link, NOW)
        .unwrap();
    store.delete_url(Some("/p"), "https://a.test/").unwrap();
    assert!(
        store
            .entries(Some("/p"), "", Order::Recent, 10)
            .unwrap()
            .is_empty()
    );

    store
        .record("/p", "https://a.test/", "", Transition::Link, NOW)
        .unwrap();
    store.clear(Some("/q")).unwrap();
    assert_eq!(store.entries(None, "", Order::Recent, 10).unwrap().len(), 1);
    store.clear(None).unwrap();
    assert!(
        store
            .visits("default", &Query::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn visits_older_than_the_retention_go_when_a_store_opens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default.db");
    {
        let store = Store::open(&path, NOW).unwrap();
        store
            .record(
                "/p",
                "https://old.test/",
                "",
                Transition::Link,
                NOW - 91 * DAY,
            )
            .unwrap();
        store
            .record(
                "/p",
                "https://new.test/",
                "",
                Transition::Link,
                NOW - 89 * DAY,
            )
            .unwrap();
    }
    let store = Store::open(&path, NOW).unwrap();
    let urls: Vec<String> = store
        .entries(None, "", Order::Recent, 10)
        .unwrap()
        .into_iter()
        .map(|e| e.url)
        .collect();
    assert_eq!(urls, ["https://new.test/"]);
}

#[test]
fn stores_are_one_file_per_profile_and_searched_together() {
    let dir = tempfile::tempdir().unwrap();
    let mut stores = Stores::new(Some(dir.path().to_owned()));
    stores
        .store(DEFAULT_PROFILE, NOW)
        .unwrap()
        .record("/p", "https://a.test/", "", Transition::Link, NOW - 10)
        .unwrap();
    stores
        .store("iso-0123456789abcdef", NOW)
        .unwrap()
        .record("/q", "https://b.test/", "", Transition::Link, NOW)
        .unwrap();
    // A name cannot reach outside the folder.
    stores.store("../evil", NOW).unwrap();
    let mut files: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".db"))
        .collect();
    files.sort();
    assert_eq!(
        files,
        ["---evil.db", "default.db", "iso-0123456789abcdef.db"]
    );

    // Another run finds the stores on disk.
    let mut again = Stores::new(Some(dir.path().to_owned()));
    let all = again.visits(&Query::default(), NOW);
    assert_eq!(
        all.iter().map(|v| v.url.as_str()).collect::<Vec<_>>(),
        ["https://b.test/", "https://a.test/"]
    );
    let only_p = again.visits(
        &Query {
            project: Some("/p".into()),
            ..Default::default()
        },
        NOW,
    );
    assert_eq!(only_p.len(), 1);
    again.delete_visit(&all[0], NOW).unwrap();
    assert_eq!(again.visits(&Query::default(), NOW).len(), 1);
    again.clear(None, NOW).unwrap();
    assert!(again.visits(&Query::default(), NOW).is_empty());
}

#[test]
fn suggestions_rank_this_projects_pages_first_then_by_frecency() {
    let mut stores = Stores::new(None);
    let store = stores.store(DEFAULT_PROFILE, NOW).unwrap();
    // Another project's page, visited often.
    for i in 0..10 {
        store
            .record("/q", "https://github.com/q", "", Transition::Typed, NOW - i)
            .unwrap();
    }
    // This project's: one seen once long ago, one typed often lately.
    store
        .record(
            "/p",
            "https://github.com/old",
            "",
            Transition::Link,
            NOW - 60 * DAY,
        )
        .unwrap();
    for i in 0..3 {
        store
            .record("/p", "https://github.com/p", "", Transition::Typed, NOW - i)
            .unwrap();
    }
    store
        .record(
            "/p",
            "https://example.com/?ref=github",
            "",
            Transition::Link,
            NOW,
        )
        .unwrap();
    let urls: Vec<String> = stores
        .suggest("/p", "github", 10, NOW)
        .into_iter()
        .map(|e| e.url)
        .collect();
    assert_eq!(
        urls,
        [
            "https://github.com/p",
            // A match at the start of the host beats one anywhere.
            "https://github.com/old",
            "https://example.com/?ref=github",
            "https://github.com/q",
        ]
    );
    assert!(stores.suggest("/p", "  ", 10, NOW).is_empty());
    assert_eq!(stores.suggest("/p", "github", 2, NOW).len(), 2);
}

#[test]
fn recent_and_most_visited_are_per_project() {
    let mut stores = Stores::new(None);
    let store = stores.store(DEFAULT_PROFILE, NOW).unwrap();
    for i in 0..5 {
        store
            .record(
                "/p",
                "https://often.test/",
                "",
                Transition::Link,
                NOW - DAY + i,
            )
            .unwrap();
    }
    store
        .record("/p", "https://latest.test/", "", Transition::Link, NOW)
        .unwrap();
    store
        .record("/q", "https://other.test/", "", Transition::Link, NOW + 1)
        .unwrap();
    let mut urls = |order| {
        stores
            .entries(Some("/p"), "", order, 10, NOW)
            .into_iter()
            .map(|e| e.url)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        urls(Order::Recent),
        ["https://latest.test/", "https://often.test/"]
    );
    assert_eq!(
        urls(Order::MostVisited),
        ["https://often.test/", "https://latest.test/"]
    );
}
