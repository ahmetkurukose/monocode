use super::*;
use gpui::{TestAppContext, WindowHandle};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::TcpListener;

const SHOP: &str = r#"{
	"info": { "name": "Shop", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json" },
	"variable": [ { "key": "baseUrl", "value": "http://placeholder.invalid" } ],
	"event": [ { "listen": "prerequest", "script": { "exec": [ "console.log(1)" ] } } ],
	"item": [
		{
			"name": "Products",
			"item": [
				{
					"name": "List Products",
					"request": {
						"method": "GET",
						"header": [],
						"url": { "raw": "{{baseUrl}}/products?limit=10&skip=0" }
					}
				},
				{ "name": "Get Product", "request": { "method": "GET", "url": "{{baseUrl}}/products/:id" } }
			]
		},
		{ "name": "Login", "request": { "method": "POST", "url": "{{baseUrl}}/login" } }
	]
}"#;

struct Fixture {
    _project: tempfile::TempDir,
    _state: tempfile::TempDir,
    state_dir: PathBuf,
    project: String,
    file: PathBuf,
    window: WindowHandle<ApiView>,
}

fn open_view(cx: &mut TestAppContext, state_dir: PathBuf) -> WindowHandle<ApiView> {
    cx.update(|cx| {
        cx.open_window(Default::default(), move |_, cx| {
            cx.new(|cx| ApiView::new(state_dir, cx))
        })
        .unwrap()
    })
}

fn open(cx: &mut TestAppContext) -> Fixture {
    cx.update(input::bind);
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let dir = api_dir(&root);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("shop.postman_collection.json");
    std::fs::write(&file, SHOP).unwrap();
    let state_dir = state.path().to_path_buf();
    let window = open_view(cx, state_dir.clone());
    window
        .update(cx, |view, _, cx| {
            view.set_scope(Some(root.clone()), vec![root.clone()], root.clone(), cx)
        })
        .unwrap();
    cx.refresh().unwrap();
    Fixture {
        state_dir,
        project: root,
        file,
        window,
        _project: project,
        _state: state,
    }
}

fn names(view: &ApiView, cx: &gpui::App) -> Vec<String> {
    view.rows(cx)
        .iter()
        .map(|row| match row {
            TreeRow::Collection(ci) => view.collections[*ci].collection.name(),
            TreeRow::Entry(_, entry) => {
                format!("{}{}", "  ".repeat(entry.depth + 1), entry.name)
            }
            TreeRow::Note(_, text) => text.clone(),
        })
        .collect()
}

fn open_first(f: &Fixture, cx: &mut TestAppContext, path: ItemPath) -> u64 {
    let file = f.file.clone();
    f.window
        .update(cx, |view, window, cx| {
            view.open_request(&file, path, window, cx);
            view.active.unwrap()
        })
        .unwrap()
}

#[gpui::test]
fn the_projects_collections_are_listed_as_a_tree_and_collapse(cx: &mut TestAppContext) {
    let f = open(cx);
    f.window
        .update(cx, |view, _, cx| {
            assert_eq!(
                names(view, cx),
                [
                    "Shop",
                    "  Products",
                    "    List Products",
                    "    Get Product",
                    "  Login"
                ]
            );
            let key = ApiView::folder_key(&view.collections[0].file, &[0]);
            view.toggle(key, cx);
            assert_eq!(names(view, cx), ["Shop", "  Products", "  Login"]);
            view.toggle(ApiView::collection_key(&view.collections[0].file), cx);
            assert_eq!(names(view, cx), ["Shop"]);
        })
        .unwrap();
    // The filter shows matching requests under their folders, collapsed or not.
    f.window
        .update(cx, |view, _, cx| {
            view.filter
                .update(cx, |i, cx| i.set_text("login".into(), cx));
            assert_eq!(names(view, cx), ["Shop", "  Login"]);
            view.filter
                .update(cx, |i, cx| i.set_text("get product".into(), cx));
            assert_eq!(names(view, cx), ["Shop", "  Products", "    Get Product"]);
            // A method matches too.
            view.filter
                .update(cx, |i, cx| i.set_text("post".into(), cx));
            assert_eq!(names(view, cx), ["Shop", "  Login"]);
        })
        .unwrap();
}

#[gpui::test]
fn the_url_bar_and_the_query_table_edit_each_other(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            assert_eq!(
                text_of(&tab.url, cx),
                "{{baseUrl}}/products?limit=10&skip=0"
            );
            assert_eq!(tab.query.len(), 2);
            assert!(!tab.dirty());
            // Typing in the bar rebuilds the table.
            tab.url.update(cx, |i, cx| {
                i.set_text("{{baseUrl}}/products?limit=25&skip=0&q=pen".into(), cx)
            });
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            assert_eq!(tab.query.len(), 3);
            assert_eq!(text_of(&tab.query[2].key, cx), "q");
            assert!(tab.dirty());
            // Editing a table value rewrites the bar.
            let value = tab.query[0].value.clone();
            value.update(cx, |i, cx| i.set_text("50".into(), cx));
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            assert_eq!(
                text_of(&tab.url, cx),
                "{{baseUrl}}/products?limit=50&skip=0&q=pen"
            );
            assert_eq!(tab.draft.url_text(), text_of(&tab.url, cx));
        })
        .unwrap();
}

#[gpui::test]
fn rows_are_added_switched_off_and_removed(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    f.window
        .update(cx, |view, _, cx| {
            view.add_row(id, Table::Headers, cx);
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            assert_eq!(tab.headers.len(), 1);
            tab.headers[0]
                .key
                .update(cx, |i, cx| i.set_text("X-Trace".into(), cx));
            tab.headers[0]
                .value
                .update(cx, |i, cx| i.set_text("1".into(), cx));
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            assert_eq!(
                view.tab(id).unwrap().draft.headers,
                vec![KeyValue::new("X-Trace", "1")]
            );
            view.row_edit(id, Table::Headers, 0, |d, i| d[i].disabled = true, cx);
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            assert!(view.tab(id).unwrap().draft.headers[0].disabled);
            view.row_edit(
                id,
                Table::Headers,
                0,
                |d, i| {
                    d.remove(i);
                },
                cx,
            );
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, _| {
            let tab = view.tab(id).unwrap();
            assert!(tab.draft.headers.is_empty() && tab.headers.is_empty());
            assert!(!tab.dirty(), "back to what was saved");
        })
        .unwrap();
}

#[gpui::test]
fn saving_changes_one_request_and_keeps_the_rest_of_the_file(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    f.window
        .update(cx, |view, _, cx| {
            view.set_method(id, "DELETE", cx);
            let tab = view.tab(id).unwrap();
            tab.url
                .update(cx, |i, cx| i.set_text("{{baseUrl}}/products/1".into(), cx));
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            assert!(view.tab(id).unwrap().dirty());
            assert!(view.save(id, cx));
            assert!(!view.tab(id).unwrap().dirty());
        })
        .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.file).unwrap()).unwrap();
    assert_eq!(saved["item"][0]["item"][0]["request"]["method"], "DELETE");
    assert_eq!(
        saved["item"][0]["item"][0]["request"]["url"]["raw"],
        "{{baseUrl}}/products/1"
    );
    assert_eq!(saved["event"][0]["listen"], "prerequest");
    assert_eq!(saved["item"][1]["name"], "Login");
}

#[gpui::test]
fn pasting_a_curl_command_into_the_url_bar_fills_the_request_in(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![1]);
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            tab.url.update(cx, |i, cx| {
                i.set_text(
                    "curl -X PUT 'https://api.test/items/9?force=1' -H 'Content-Type: application/json' -H 'Authorization: Bearer abc' --data-raw '{\"n\":1}'".into(),
                    cx,
                )
            });
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            assert_eq!(tab.draft.method, "PUT");
            assert_eq!(tab.draft.name, "Login", "the request keeps its name");
            assert_eq!(text_of(&tab.url, cx), "https://api.test/items/9?force=1");
            assert_eq!(
                tab.draft.auth,
                Auth::Bearer {
                    token: "abc".into()
                }
            );
            assert_eq!(text_of(&tab.auth.token, cx), "abc");
            assert_eq!(text_of(&tab.body, cx), "{\"n\":1}");
            assert!(tab.body_mode == BodyMode::Raw);
            assert_eq!(tab.query.len(), 1);
            assert!(tab.dirty());
            assert!(
                view.notice
                    .as_deref()
                    .is_some_and(|n| n.contains("Imported"))
            );
        })
        .unwrap();
}

#[gpui::test]
fn the_body_and_auth_tabs_change_what_is_sent(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![1]);
    f.window
        .update(cx, |view, _, cx| {
            view.set_body_mode(id, BodyMode::Raw, cx);
            view.set_raw_language(id, RawLanguage::Json, cx);
            let body = view.tab(id).unwrap().body.clone();
            body.update(cx, |i, cx| i.set_text("{\"a\":1}".into(), cx));
            view.set_auth_kind(id, "basic", cx);
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            tab.auth
                .username
                .update(cx, |i, cx| i.set_text("me".into(), cx));
            tab.auth
                .password
                .update(cx, |i, cx| i.set_text("pw".into(), cx));
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, _| {
            let tab = view.tab(id).unwrap();
            assert_eq!(
                tab.draft.body,
                Body::Raw {
                    text: "{\"a\":1}".into(),
                    language: RawLanguage::Json
                }
            );
            assert_eq!(
                tab.draft.auth,
                Auth::Basic {
                    username: "me".into(),
                    password: "pw".into()
                }
            );
            let prepared = view.prepare_tab(tab);
            assert_eq!(prepared.header("content-type"), Some("application/json"));
            assert_eq!(prepared.header("authorization"), Some("Basic bWU6cHc="));
        })
        .unwrap();
    // Switching the mode away and back keeps what was typed.
    f.window
        .update(cx, |view, _, cx| {
            view.set_body_mode(id, BodyMode::None, cx);
            assert_eq!(view.tab(id).unwrap().draft.body, Body::None);
            view.set_body_mode(id, BodyMode::Raw, cx);
            assert_eq!(
                view.tab(id).unwrap().draft.body,
                Body::Raw {
                    text: "{\"a\":1}".into(),
                    language: RawLanguage::Json
                }
            );
        })
        .unwrap();
}

#[gpui::test]
fn a_request_inherits_the_environment_and_collection_variables(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    let env = api_dir(&f.project).join("local.postman_environment.json");
    std::fs::write(
        &env,
        r#"{"id":"e","name":"local","values":[{"key":"baseUrl","value":"http://127.0.0.1:9","enabled":true}]}"#,
    )
    .unwrap();
    f.window
        .update(cx, |view, _, cx| {
            let before = view.prepare_tab(view.tab(id).unwrap());
            assert!(
                before
                    .url
                    .starts_with("http://placeholder.invalid/products")
            );
            view.reload(cx);
            assert_eq!(view.environments.len(), 1);
            view.select_environment(&f.project, Some(env.clone()));
            let after = view.prepare_tab(view.tab(id).unwrap());
            assert_eq!(after.url, "http://127.0.0.1:9/products?limit=10&skip=0");
            assert!(after.missing.is_empty());
        })
        .unwrap();
}

/// Serves one canned answer and reports the request line it saw.
fn serve(reply: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut received = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).unwrap();
            received.extend_from_slice(&chunk[..n]);
            if n == 0 || received.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        stream.write_all(reply.as_bytes()).unwrap();
        String::from_utf8_lossy(&received)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned()
    });
    (url, handle)
}

#[gpui::test]
fn sending_shows_the_response_and_a_newer_send_wins(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    let (url, server) = serve(
        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"id\":1,\"a\":2}",
    );
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            tab.url
                .update(cx, |i, cx| i.set_text(format!("{url}/made?x=1"), cx));
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| view.send(id, cx))
        .unwrap();
    for _ in 0..200 {
        cx.run_until_parked();
        let done = f
            .window
            .update(cx, |view, _, _| {
                matches!(view.tab(id).unwrap().exchange, Exchange::Done(_))
            })
            .unwrap();
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.join().unwrap(), "GET /made?x=1 HTTP/1.1");
    cx.refresh().unwrap();
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            let Exchange::Done(done) = &tab.exchange else {
                panic!("no response");
            };
            assert_eq!((done.status, done.reason.as_str()), (201, "Created"));
            assert!(done.pretty.contains("\"id\": 1"), "{}", done.pretty);
            assert_eq!(done.syntax, "response.json");
            assert!(text_of(&tab.viewer, cx).contains("\"id\": 1"));
        })
        .unwrap();
    // Switching to Raw shows the bytes as they came.
    f.window
        .update(cx, |view, _, cx| {
            view.set_response_tab(id, ResponseTab::Raw, cx)
        })
        .unwrap();
    f.window
        .update(cx, |view, _, cx| {
            assert_eq!(
                text_of(&view.tab(id).unwrap().viewer, cx),
                "{\"id\":1,\"a\":2}"
            );
        })
        .unwrap();
}

#[gpui::test]
fn cancelling_drops_the_answer_that_arrives_later(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab_mut(id).unwrap();
            tab.exchange = Exchange::Sending;
            tab.generation = 5;
            view.cancel(id, cx);
            assert!(matches!(view.tab(id).unwrap().exchange, Exchange::Idle));
            let late = api::ExchangeOutput {
                outcome: Ok(api::Response {
                    status: 200,
                    reason: "OK".into(),
                    headers: Vec::new(),
                    body: b"late".to_vec(),
                    truncated: false,
                    elapsed: Duration::from_millis(1),
                    url: String::new(),
                }),
                request: ApiRequest::default(),
                prepared: None,
                tests: Vec::new(),
                logs: Vec::new(),
                environment: Vec::new(),
                collection: Vec::new(),
                globals: Vec::new(),
            };
            view.finish(id, 5, Some(late), cx);
            assert!(matches!(view.tab(id).unwrap().exchange, Exchange::Idle));
        })
        .unwrap();
}

#[gpui::test]
fn new_items_rename_duplicate_and_delete_keep_open_tabs_pointing_at_their_requests(
    cx: &mut TestAppContext,
) {
    let f = open(cx);
    let login = open_first(&f, cx, vec![1]);
    let file = f.file.clone();
    f.window
        .update(cx, |view, window, cx| {
            view.new_item(&file, &[0], false, window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            // The new request opened and its name is being asked for.
            assert!(matches!(view.dialog, Some(DialogKind::Rename(_))));
            set_text(&view.dialog_input, "Create Product", cx);
            view.confirm_dialog(cx);
            assert_eq!(
                names(view, cx),
                [
                    "Shop",
                    "  Products",
                    "    List Products",
                    "    Get Product",
                    "    Create Product",
                    "  Login"
                ]
            );
            // Login was at [1] and is untouched; the new tab is at [0, 2].
            assert_eq!(view.tab(login).unwrap().path, vec![1]);
            let created = view.active.unwrap();
            assert_eq!(view.tab(created).unwrap().path, vec![0, 2]);
            assert_eq!(view.tab(created).unwrap().draft.name, "Create Product");

            // Duplicating the folder pushes Login down one place.
            view.duplicate(
                &Target {
                    file: file.clone(),
                    path: vec![0],
                },
                cx,
            );
            assert_eq!(view.tab(login).unwrap().path, vec![2]);
            assert_eq!(view.tab(created).unwrap().path, vec![0, 2]);

            // Deleting the original folder closes the tab inside it and
            // moves Login up again.
            view.delete(
                &Target {
                    file: file.clone(),
                    path: vec![0],
                },
                cx,
            );
            assert!(view.tab(created).is_none());
            assert_eq!(view.tab(login).unwrap().path, vec![1]);
            assert_eq!(view.active, Some(login));
            assert_eq!(
                view.collections[0].collection.request(&[1]).unwrap().name,
                "Login"
            );
        })
        .unwrap();
    let on_disk = Collection::load(&f.file).unwrap();
    assert_eq!(on_disk.request_count(), 4);
}

#[gpui::test]
fn closing_a_changed_tab_asks_first(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![1]);
    f.window
        .update(cx, |view, window, cx| {
            view.set_method(id, "PATCH", cx);
            view.close_tab(id, window, cx);
            assert!(view.tab(id).is_some());
            assert!(matches!(view.dialog, Some(DialogKind::CloseDirty(i)) if i == id));
            view.dialog = None;
            view.revert(id, cx);
            assert_eq!(view.tab(id).unwrap().draft.method, "POST");
            view.close_tab(id, window, cx);
            assert!(view.tab(id).is_none() && view.active.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn a_new_collection_and_an_import_land_in_the_project(cx: &mut TestAppContext) {
    let f = open(cx);
    let root = f.project.clone();
    f.window
        .update(cx, |view, window, cx| {
            view.start_new_collection(window, cx);
            set_text(&view.dialog_input, "Billing API", cx);
            view.confirm_dialog(cx);
            assert_eq!(view.collections.len(), 2);
        })
        .unwrap();
    assert!(
        api_dir(&root)
            .join("billing-api.postman_collection.json")
            .exists()
    );
    let outside = tempfile::tempdir().unwrap();
    let coll = outside.path().join("Export.json");
    std::fs::write(&coll, SHOP.replace("\"Shop\"", "\"Imported\"")).unwrap();
    let env = outside.path().join("Staging.json");
    std::fs::write(&env, r#"{"name":"staging","values":[]}"#).unwrap();
    let junk = outside.path().join("junk.json");
    std::fs::write(&junk, "{}").unwrap();
    f.window
        .update(cx, |view, _, cx| {
            view.import_files(&root, &[coll.clone(), env.clone(), junk.clone()], cx);
            assert_eq!(view.collections.len(), 3);
            assert_eq!(view.environments.len(), 1);
            assert!(
                view.notice
                    .as_deref()
                    .is_some_and(|n| n.contains("Imported 2"))
            );
            assert!(
                view.notice
                    .as_deref()
                    .is_some_and(|n| n.contains("junk.json"))
            );
        })
        .unwrap();
}

#[gpui::test]
fn every_section_and_the_response_panel_render(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    for section in [
        Section::Path,
        Section::Query,
        Section::Headers,
        Section::Body,
        Section::Auth,
        Section::Notes,
    ] {
        f.window
            .update(cx, |view, _, cx| {
                view.tab_mut(id).unwrap().section = section;
                cx.notify();
            })
            .unwrap();
        cx.refresh().unwrap();
    }
    for mode in [
        BodyMode::Raw,
        BodyMode::UrlEncoded,
        BodyMode::Form,
        BodyMode::None,
    ] {
        f.window
            .update(cx, |view, _, cx| {
                view.tab_mut(id).unwrap().section = Section::Body;
                view.set_body_mode(id, mode, cx);
            })
            .unwrap();
        cx.refresh().unwrap();
    }
    for kind in ["none", "basic", "bearer", "apikey", "inherit"] {
        f.window
            .update(cx, |view, _, cx| {
                view.tab_mut(id).unwrap().section = Section::Auth;
                view.set_auth_kind(id, kind, cx);
            })
            .unwrap();
        cx.refresh().unwrap();
    }
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab_mut(id).unwrap();
            tab.exchange = Exchange::Failed("Could not connect.".into());
            cx.notify();
        })
        .unwrap();
    cx.refresh().unwrap();
}

fn add_environment(f: &Fixture, name: &str, body: &str) -> PathBuf {
    let file = api_dir(&f.project).join(format!("{name}.postman_environment.json"));
    std::fs::write(&file, body).unwrap();
    file
}

fn type_row(view: &ApiView, ix: usize, key: &str, value: &str, cx: &mut Context<ApiView>) {
    let row = &view.env_editor.as_ref().unwrap().rows[ix];
    row.key.update(cx, |i, cx| i.set_text(key.into(), cx));
    row.value.update(cx, |i, cx| i.set_text(value.into(), cx));
}

#[gpui::test]
fn the_environment_editor_changes_variables_and_requests_use_them(cx: &mut TestAppContext) {
    let f = open(cx);
    let file = add_environment(
        &f,
        "local",
        r#"{"id":"e","name":"local","values":[{"key":"baseUrl","value":"http://one.test","type":"default","enabled":true}],"_postman_exported_at":"x"}"#,
    );
    let id = open_first(&f, cx, vec![0, 0]);
    f.window
        .update(cx, |view, _, cx| {
            view.reload(cx);
            view.open_env_editor(None, cx);
            let editor = view.env_editor.as_ref().unwrap();
            assert!(editor.target == EnvTarget::Environment(file.clone()));
            assert_eq!(editor.rows.len(), 1);
            assert!(!editor.dirty);
            view.use_environment(file.clone(), cx);
            view.add_env_row(cx);
            assert_eq!(view.env_editor.as_ref().unwrap().rows.len(), 2);
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            type_row(view, 0, "baseUrl", "http://two.test", cx);
            type_row(view, 1, "token", "s3cret", cx);
            view.toggle_env_secret(1, cx);
            let name = view.env_editor.as_ref().unwrap().name.clone();
            name.update(cx, |i, cx| i.set_text("staging".into(), cx));
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            assert!(view.env_editor.as_ref().unwrap().dirty);
            // Nothing is on disk until it is saved.
            assert!(std::fs::read_to_string(&file).unwrap().contains("one.test"));
            view.close_env_editor(true, cx);
            assert!(view.env_editor.is_none());
            let prepared = view.prepare_tab(view.tab(id).unwrap());
            assert!(prepared.url.starts_with("http://two.test/products"));
        })
        .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["name"], "staging");
    assert_eq!(saved["values"][0]["value"], "http://two.test");
    assert_eq!(saved["values"][1]["key"], "token");
    assert_eq!(saved["values"][1]["type"], "secret");
    assert_eq!(
        saved["_postman_exported_at"], "x",
        "export metadata is kept"
    );
}

#[gpui::test]
fn rows_can_be_switched_off_removed_and_a_cancelled_edit_changes_nothing(cx: &mut TestAppContext) {
    let f = open(cx);
    let file = add_environment(
        &f,
        "e",
        r#"{"name":"e","values":[{"key":"a","value":"1","enabled":true},{"key":"b","value":"2","enabled":true}]}"#,
    );
    f.window
        .update(cx, |view, _, cx| {
            view.reload(cx);
            view.open_env_editor(Some(EnvTarget::Environment(file.clone())), cx);
            view.env_row_edit(0, |rows, i| rows[i].enabled = false, cx);
            view.env_row_edit(
                1,
                |rows, i| {
                    rows.remove(i);
                },
                cx,
            );
            view.close_env_editor(false, cx);
        })
        .unwrap();
    assert!(std::fs::read_to_string(&file).unwrap().contains("\"b\""));
    f.window
        .update(cx, |view, _, cx| {
            view.open_env_editor(Some(EnvTarget::Environment(file.clone())), cx);
            view.env_row_edit(0, |rows, i| rows[i].enabled = false, cx);
            view.env_row_edit(
                1,
                |rows, i| {
                    rows.remove(i);
                },
                cx,
            );
            view.close_env_editor(true, cx);
            let env = view.selected_environment();
            assert!(env.is_none(), "saving does not select it");
            let loaded = &view.environments[0].environment;
            assert_eq!(loaded.values().len(), 1);
            assert!(!loaded.values()[0].enabled);
            assert!(
                loaded.variables().is_empty(),
                "a switched-off row is not a variable"
            );
        })
        .unwrap();
}

#[gpui::test]
fn a_new_environment_is_made_selected_and_can_be_deleted(cx: &mut TestAppContext) {
    let f = open(cx);
    f.window
        .update(cx, |view, _, cx| {
            // With a collection but no environment, the editor starts on the
            // collection's variables.
            view.open_env_editor(None, cx);
            assert!(matches!(
                view.env_editor.as_ref().unwrap().target,
                EnvTarget::Variables(_)
            ));
            view.new_environment(cx);
            assert_eq!(view.environments.len(), 1);
            assert_eq!(view.environments[0].environment.name(), "New Environment");
            assert!(view.selected_environment().is_some());
            assert!(matches!(
                view.env_editor.as_ref().unwrap().target,
                EnvTarget::Environment(_)
            ));
            view.new_environment(cx);
            assert_eq!(view.environments.len(), 2);
        })
        .unwrap();
    let files: Vec<_> = std::fs::read_dir(api_dir(&f.project))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(ENVIRONMENT_SUFFIX))
        .collect();
    assert_eq!(files.len(), 2);
    f.window
        .update(cx, |view, _, cx| {
            view.delete_environment(cx);
            assert_eq!(view.environments.len(), 1);
            assert!(view.env_editor.is_some(), "it moves on to what is left");
            view.delete_environment(cx);
            assert!(view.environments.is_empty());
            assert!(view.environment.is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn collection_variables_are_edited_in_place_and_switching_saves_first(cx: &mut TestAppContext) {
    let f = open(cx);
    let file = add_environment(&f, "e", r#"{"name":"e","values":[]}"#);
    f.window
        .update(cx, |view, _, cx| {
            view.reload(cx);
            view.open_env_editor(
                Some(EnvTarget::Variables(view.collections[0].file.clone())),
                cx,
            );
            view.add_env_row(cx);
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            let rows = view.env_editor.as_ref().unwrap().rows.len();
            assert_eq!(rows, 2, "the baseUrl it came with and the new one");
            type_row(view, 1, "page", "2", cx);
            // Going to another target saves what was typed.
            view.switch_env_target(EnvTarget::Environment(file.clone()), cx);
            assert!(matches!(
                view.env_editor.as_ref().unwrap().target,
                EnvTarget::Environment(_)
            ));
        })
        .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.file).unwrap()).unwrap();
    assert_eq!(saved["variable"][0]["key"], "baseUrl");
    assert_eq!(saved["variable"][1]["key"], "page");
    assert_eq!(
        saved["event"][0]["listen"], "prerequest",
        "the rest of the file stays"
    );
}

#[gpui::test]
fn the_environment_editor_renders_for_each_target(cx: &mut TestAppContext) {
    let f = open(cx);
    let file = add_environment(
        &f,
        "e",
        r#"{"name":"e","values":[{"key":"a","value":"1","type":"secret","enabled":true}]}"#,
    );
    f.window
        .update(cx, |view, _, cx| {
            view.reload(cx);
            view.open_env_editor(Some(EnvTarget::Environment(file.clone())), cx);
        })
        .unwrap();
    cx.refresh().unwrap();
    f.window
        .update(cx, |view, _, cx| {
            view.switch_env_target(EnvTarget::Variables(view.collections[0].file.clone()), cx);
        })
        .unwrap();
    cx.refresh().unwrap();
}

#[gpui::test]
fn a_fresh_empty_collection_can_get_a_request_from_the_menus(cx: &mut TestAppContext) {
    let f = open(cx);
    std::fs::remove_file(&f.file).unwrap();
    f.window
        .update(cx, |view, window, cx| {
            view.reload(cx);
            assert!(view.collections.is_empty());
            view.start_new_collection(window, cx);
            set_text(&view.dialog_input, "Mine", cx);
            view.confirm_dialog(cx);
            assert_eq!(
                names(view, cx),
                ["Mine", "No requests yet. Use + to add one."]
            );
            // The header's + menu offers it now.
            view.run_menu_action("request", MenuKind::Add, window, cx);
            assert!(matches!(view.dialog, Some(DialogKind::Rename(_))));
            set_text(&view.dialog_input, "Ping", cx);
            view.confirm_dialog(cx);
            assert_eq!(names(view, cx), ["Mine", "  Ping"]);
            view.run_menu_action("folder", MenuKind::Add, window, cx);
            set_text(&view.dialog_input, "Users", cx);
            view.confirm_dialog(cx);
            assert_eq!(names(view, cx), ["Mine", "  Ping", "  Users"]);
        })
        .unwrap();
    cx.refresh().unwrap();
}

#[gpui::test]
fn an_empty_url_variable_is_called_empty_not_a_scheme_problem(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            tab.url
                .update(cx, |i, cx| i.set_text("{{url}}/flow/history".into(), cx));
            let file = view.collections[0].file.clone();
            view.collection_mut(&file)
                .unwrap()
                .collection
                .set_variables(&[KeyValue::new("url", "")]);
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, _| {
            let prepared = view.prepare_tab(view.tab(id).unwrap());
            assert_eq!(prepared.url, "/flow/history");
            assert_eq!(prepared.empty, vec!["url"]);
            let error = api::send(&prepared, &SendOptions::default()).unwrap_err();
            assert!(error.to_string().contains("{{url}} is empty"), "{error}");
        })
        .unwrap();
    cx.refresh().unwrap();
}

// ---- scripts ----

#[gpui::test]
fn a_test_script_saves_the_token_it_finds_into_the_chosen_environment(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let f = open(cx);
    let env = add_environment(&f, "local", r#"{"name":"local","values":[]}"#);
    let id = open_first(&f, cx, vec![0, 0]);
    let (url, server) = serve(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"token\":\"t0k\"}",
    );
    f.window
        .update(cx, |view, _, cx| {
            view.reload(cx);
            view.select_environment(&f.project, Some(env.clone()));
            let tab = view.tab(id).unwrap();
            tab.url.update(cx, |i, cx| i.set_text(format!("{url}/login"), cx));
            tab.test_script.update(cx, |i, cx| {
                i.set_text(
                    "pm.test('has token', () => pm.expect(pm.response.json().token).to.be.a('string')); \
                     pm.environment.set('token', pm.response.json().token);"
                        .into(),
                    cx,
                )
            });
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| view.send(id, cx))
        .unwrap();
    for _ in 0..200 {
        cx.run_until_parked();
        let done = f
            .window
            .update(cx, |view, _, _| {
                !matches!(view.tab(id).unwrap().exchange, Exchange::Sending)
            })
            .unwrap();
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    server.join().unwrap();
    f.window
        .update(cx, |view, _, _| {
            let tab = view.tab(id).unwrap();
            assert!(
                matches!(tab.exchange, Exchange::Done(_)),
                "{}",
                matches!(tab.exchange, Exchange::Failed(_))
            );
            assert_eq!(tab.tests.len(), 1);
            assert!(tab.tests[0].passed);
            let loaded = view.environment_for(&f.project).unwrap();
            assert!(
                loaded
                    .environment
                    .values()
                    .iter()
                    .any(|v| v.key == "token" && v.value == "t0k")
            );
        })
        .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&env).unwrap()).unwrap();
    assert_eq!(saved["values"][0]["key"], "token");
    assert_eq!(saved["values"][0]["value"], "t0k");
}

#[gpui::test]
fn without_an_environment_what_scripts_set_lasts_for_the_session(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab_mut(id).unwrap();
            tab.generation = 1;
            let output = api::ExchangeOutput {
                outcome: Err(api::ApiError::new("offline")),
                request: ApiRequest::default(),
                prepared: None,
                tests: Vec::new(),
                logs: vec!["hi".into()],
                environment: vec![api::VarChange {
                    key: "token".into(),
                    value: Some("abc".into()),
                }],
                collection: vec![api::VarChange {
                    key: "page".into(),
                    value: Some("9".into()),
                }],
                globals: vec![api::VarChange {
                    key: "g".into(),
                    value: Some("1".into()),
                }],
            };
            view.finish(id, 1, Some(output), cx);
            assert_eq!(
                view.globals
                    .iter()
                    .map(|v| (v.key.as_str(), v.value.as_str()))
                    .collect::<Vec<_>>(),
                [("token", "abc"), ("g", "1")]
            );
            let tab = view.tab(id).unwrap();
            assert_eq!(tab.logs, vec!["hi"]);
            assert!(matches!(tab.exchange, Exchange::Failed(_)));
            // The collection variable went to the collection and its file.
            let scope = view.variables_scope(&tab.file, &tab.path);
            assert_eq!(scope.get("page"), Some("9"));
            assert_eq!(scope.get("token"), Some("abc"));
        })
        .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.file).unwrap()).unwrap();
    assert_eq!(saved["variable"][0]["key"], "baseUrl");
    assert!(
        saved["variable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["key"] == "page" && v["value"] == "9")
    );
}

#[gpui::test]
fn the_scripts_tab_edits_the_requests_own_scripts_and_shows_the_collections(
    cx: &mut TestAppContext,
) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            // The collection's own pre-request script runs first.
            assert_eq!(
                view.scripts_for(tab, "prerequest"),
                vec!["console.log(1)".to_owned(), String::new()]
            );
            assert_eq!(view.script_owners(tab, "prerequest"), vec!["Shop"]);
            tab.pre_script.update(cx, |i, cx| {
                i.set_text("pm.variables.set('a', 1);".into(), cx)
            });
            tab.test_script
                .update(cx, |i, cx| i.set_text("pm.test('x', () => {});".into(), cx));
        })
        .unwrap();
    cx.run_until_parked();
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab(id).unwrap();
            assert!(tab.dirty());
            assert_eq!(tab.draft.pre_request, "pm.variables.set('a', 1);");
            assert_eq!(
                view.scripts_for(tab, "prerequest").last().unwrap(),
                "pm.variables.set('a', 1);"
            );
            view.tab_mut(id).unwrap().section = Section::Scripts;
            cx.notify();
            assert!(view.save(id, cx));
        })
        .unwrap();
    cx.refresh().unwrap();
    f.window
        .update(cx, |view, _, cx| {
            view.tab_mut(id).unwrap().script_kind = ScriptKind::Tests;
            view.tab_mut(id).unwrap().exchange = Exchange::Idle;
            cx.notify();
        })
        .unwrap();
    cx.refresh().unwrap();
    let saved = Collection::load(&f.file).unwrap();
    let request = saved.request(&[0, 0]).unwrap();
    assert_eq!(request.pre_request, "pm.variables.set('a', 1);");
    assert_eq!(request.tests, "pm.test('x', () => {});");
}

// ---- tabs ----

fn open_all(f: &Fixture, cx: &mut TestAppContext) -> Vec<u64> {
    let file = f.file.clone();
    f.window
        .update(cx, |view, window, cx| {
            let extra = view.collections[0].collection.clone();
            drop(extra);
            for path in [vec![0, 0], vec![0, 1], vec![1]] {
                view.open_request(&file, path, window, cx);
            }
            view.tabs.iter().map(|t| t.id).collect()
        })
        .unwrap()
}

#[gpui::test]
fn tabs_close_in_groups_and_ask_once_when_something_is_unsaved(cx: &mut TestAppContext) {
    let f = open(cx);
    let ids = open_all(&f, cx);
    f.window
        .update(cx, |view, window, cx| {
            assert_eq!(view.tabs.len(), 3);
            // To the right of the first, then the left of the last.
            view.close_right(ids[0], window, cx);
            assert_eq!(
                view.tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
                vec![ids[0]]
            );
        })
        .unwrap();
    let ids = open_all(&f, cx);
    f.window
        .update(cx, |view, window, cx| {
            view.close_left(ids[2], window, cx);
            assert_eq!(view.tabs.len(), 1);
            assert_eq!(view.active, Some(ids[2]));
        })
        .unwrap();
    let ids = open_all(&f, cx);
    f.window
        .update(cx, |view, window, cx| {
            // One has changes: closing the others asks, and nothing closes yet.
            view.set_method(ids[1], "PUT", cx);
            view.close_others(ids[0], window, cx);
            assert_eq!(view.tabs.len(), 3);
            assert!(matches!(&view.dialog, Some(DialogKind::CloseMany(list)) if list.len() == 2));
            // Close saved keeps the one with changes, with no question.
            view.dialog = None;
            view.close_saved(window, cx);
            assert_eq!(
                view.tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
                vec![ids[1]]
            );
            assert!(view.dialog.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn close_all_can_save_everything_first_or_discard(cx: &mut TestAppContext) {
    let f = open(cx);
    let ids = open_all(&f, cx);
    f.window
        .update(cx, |view, window, cx| {
            view.set_method(ids[0], "PUT", cx);
            view.set_method(ids[2], "PATCH", cx);
            assert_eq!(view.dirty_ids(), vec![ids[0], ids[2]]);
            view.close_all(window, cx);
            assert!(matches!(view.dialog, Some(DialogKind::CloseMany(_))));
            // Save all writes both requests.
            assert!(view.save_all(cx));
            assert!(view.dirty_ids().is_empty());
            view.dialog = None;
            view.close_all(window, cx);
            assert!(view.tabs.is_empty() && view.active.is_none());
        })
        .unwrap();
    let saved = Collection::load(&f.file).unwrap();
    assert_eq!(saved.request(&[0, 0]).unwrap().method, "PUT");
    assert_eq!(saved.request(&[1]).unwrap().method, "PATCH");
    // Discarding throws the changes away.
    let ids = open_all(&f, cx);
    f.window
        .update(cx, |view, window, cx| {
            view.set_method(ids[1], "DELETE", cx);
            view.close_all(window, cx);
            let Some(DialogKind::CloseMany(list)) = view.dialog.take() else {
                panic!("no question asked");
            };
            view.discard_many(list, window, cx);
            assert!(view.tabs.is_empty());
        })
        .unwrap();
    assert_eq!(
        Collection::load(&f.file)
            .unwrap()
            .request(&[0, 1])
            .unwrap()
            .method,
        "GET"
    );
}

#[gpui::test]
fn a_closed_tab_comes_back_and_tabs_can_be_stepped_through(cx: &mut TestAppContext) {
    let f = open(cx);
    let ids = open_all(&f, cx);
    f.window
        .update(cx, |view, window, cx| {
            view.close_tab(ids[2], window, cx);
            view.close_tab(ids[1], window, cx);
            assert_eq!(view.tabs.len(), 1);
            view.reopen_closed(window, cx);
            assert_eq!(view.tabs.len(), 2);
            assert_eq!(
                view.active_tab().unwrap().path,
                vec![0, 1],
                "the last one closed"
            );
            view.reopen_closed(window, cx);
            assert_eq!(view.tabs.len(), 3);
            view.reopen_closed(window, cx);
            assert!(
                view.notice
                    .as_deref()
                    .is_some_and(|n| n.contains("No closed"))
            );
            // Stepping wraps, and the numbers go to a place.
            let order: Vec<Vec<usize>> = view.tabs.iter().map(|t| t.path.clone()).collect();
            view.activate(view.tabs[0].id, window, cx);
            view.step_tab(-1, window, cx);
            assert_eq!(view.active_tab().unwrap().path, order[2]);
            view.step_tab(1, window, cx);
            assert_eq!(view.active_tab().unwrap().path, order[0]);
            view.go_to_tab(2, window, cx);
            assert_eq!(view.active_tab().unwrap().path, order[1]);
            view.go_to_tab(9, window, cx);
            assert_eq!(
                view.active_tab().unwrap().path,
                order[2],
                "9 is the last tab"
            );
            view.go_to_tab(7, window, cx);
            assert_eq!(
                view.active_tab().unwrap().path,
                order[2],
                "no seventh tab: nothing moves"
            );
        })
        .unwrap();
}

#[gpui::test]
fn the_tab_menus_offer_what_fits(cx: &mut TestAppContext) {
    let f = open(cx);
    let ids = open_all(&f, cx);
    f.window
        .update(cx, |view, window, cx| {
            view.open_tab_menu(
                ids[0],
                gpui::point(gpui::px(10.), gpui::px(10.)),
                window,
                cx,
            );
            assert!(matches!(view.menu.as_ref().unwrap().kind, MenuKind::Tab(id) if id == ids[0]));
            view.menu = None;
            view.open_tabs_menu(gpui::point(gpui::px(10.), gpui::px(10.)), window, cx);
            assert!(matches!(view.menu.as_ref().unwrap().kind, MenuKind::Tabs));
            view.menu = None;
            view.run_tab_action(ids[1], "close", window, cx);
            assert_eq!(view.tabs.len(), 2);
            view.run_tabs_action(&format!("go:{}", ids[0]), window, cx);
            assert_eq!(view.active, Some(ids[0]));
            view.run_tabs_action("all", window, cx);
            assert!(view.tabs.is_empty());
        })
        .unwrap();
    cx.refresh().unwrap();
}

// ---- remembering and following the files ----

#[gpui::test]
fn open_tabs_come_back_after_a_restart_and_wait_for_their_project(cx: &mut TestAppContext) {
    let f = open(cx);
    let ids = open_all(&f, cx);
    f.window
        .update(cx, |view, window, cx| view.activate(ids[1], window, cx))
        .unwrap();
    let root = f.project.clone();
    // Another project shows nothing of it, and keeps the tabs for later.
    let other = tempfile::tempdir().unwrap();
    let other_root = other.path().to_string_lossy().into_owned();
    let elsewhere = open_view(cx, f.state_dir.clone());
    elsewhere
        .update(cx, |view, _, cx| {
            view.set_scope(
                Some(other_root.clone()),
                vec![other_root.clone()],
                other_root.clone(),
                cx,
            );
            assert!(view.tabs.is_empty());
            assert_eq!(view.parked.len(), 3);
            view.save_state();
        })
        .unwrap();
    drop(elsewhere);
    // Back in the first project they all reopen, the same one in front.
    let again = open_view(cx, f.state_dir.clone());
    again
        .update(cx, |view, _, cx| {
            view.set_scope(Some(root.clone()), vec![root.clone()], root.clone(), cx);
            assert_eq!(
                view.tabs.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
                vec![vec![0, 0], vec![0, 1], vec![1]]
            );
            assert_eq!(view.active_tab().unwrap().path, vec![0, 1]);
            assert!(view.parked.is_empty());
        })
        .unwrap();
}

#[gpui::test]
fn each_project_has_its_own_environment(cx: &mut TestAppContext) {
    let f = open(cx);
    let second = tempfile::tempdir().unwrap();
    let second_root = second.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(api_dir(&second_root)).unwrap();
    let second_file = api_dir(&second_root).join("shop.postman_collection.json");
    std::fs::write(&second_file, SHOP).unwrap();
    let env_one = add_environment(
        &f,
        "one",
        r#"{"name":"one","values":[{"key":"baseUrl","value":"http://one.test","enabled":true}]}"#,
    );
    let env_two = api_dir(&second_root).join("two.postman_environment.json");
    std::fs::write(
        &env_two,
        r#"{"name":"two","values":[{"key":"baseUrl","value":"http://two.test","enabled":true}]}"#,
    )
    .unwrap();
    let root = f.project.clone();
    f.window
        .update(cx, |view, window, cx| {
            view.set_scope(
                None,
                vec![root.clone(), second_root.clone()],
                root.clone(),
                cx,
            );
            assert_eq!(view.collections.len(), 2);
            view.select_environment(&root, Some(env_one.clone()));
            view.select_environment(&second_root, Some(env_two.clone()));
            view.open_request(&f.file, vec![0, 0], window, cx);
            view.open_request(&second_file, vec![0, 0], window, cx);
            let (a, b) = (view.tabs[0].id, view.tabs[1].id);
            assert!(
                view.prepare_tab(view.tab(a).unwrap())
                    .url
                    .starts_with("http://one.test")
            );
            assert!(
                view.prepare_tab(view.tab(b).unwrap())
                    .url
                    .starts_with("http://two.test")
            );
            // The label follows the front tab's project.
            assert_eq!(
                view.selected_environment().unwrap().environment.name(),
                "two"
            );
            view.activate(a, window, cx);
            assert_eq!(
                view.selected_environment().unwrap().environment.name(),
                "one"
            );
            assert!(view.env_in_use(&env_one) && view.env_in_use(&env_two));
        })
        .unwrap();
    // The choices are remembered per project.
    let again = open_view(cx, f.state_dir.clone());
    again
        .update(cx, |view, _, cx| {
            view.set_scope(
                None,
                vec![root.clone(), second_root.clone()],
                root.clone(),
                cx,
            );
            assert_eq!(view.environment.get(&root), Some(&env_one));
            assert_eq!(view.environment.get(&second_root), Some(&env_two));
        })
        .unwrap();
}

#[gpui::test]
fn outside_changes_are_followed_but_this_views_own_saves_are_not_mistaken_for_them(
    cx: &mut TestAppContext,
) {
    let f = open(cx);
    let clean = open_first(&f, cx, vec![0, 0]);
    let busy = open_first(&f, cx, vec![1]);
    f.window
        .update(cx, |view, _, cx| {
            assert!(!view.refresh_from_disk(cx), "nothing changed yet");
            view.set_method(busy, "PUT", cx);
            // Its own save leaves the files as this view expects them.
            assert!(view.save(busy, cx));
            assert!(
                !view.refresh_from_disk(cx),
                "own write is not an outside change"
            );
            view.set_method(busy, "PATCH", cx);
        })
        .unwrap();
    // Something else rewrites the file: the first request gets another URL.
    let mut outside = Collection::load(&f.file).unwrap();
    let mut request = outside.request(&[0, 0]).unwrap();
    request.set_url_text("{{baseUrl}}/changed");
    outside.set_request(&[0, 0], &request);
    outside.save().unwrap();
    f.window
        .update(cx, |view, _, cx| {
            assert!(view.refresh_from_disk(cx));
            let tab = view.tab(clean).unwrap();
            assert_eq!(
                tab.draft.url, "{{baseUrl}}/changed",
                "a tab with nothing typed follows the file"
            );
            assert_eq!(text_of(&tab.url, cx), "{{baseUrl}}/changed");
            assert!(!tab.dirty());
            let tab = view.tab(busy).unwrap();
            assert_eq!(tab.draft.method, "PATCH", "typed changes are kept");
            assert!(tab.dirty());
            assert!(!view.refresh_from_disk(cx), "now it matches again");
        })
        .unwrap();
}

#[gpui::test]
fn a_request_deleted_outside_closes_its_clean_tab_but_not_one_with_edits(cx: &mut TestAppContext) {
    let f = open(cx);
    let clean = open_first(&f, cx, vec![0, 1]);
    let busy = open_first(&f, cx, vec![1]);
    f.window
        .update(cx, |view, _, cx| view.set_method(busy, "PUT", cx))
        .unwrap();
    let mut outside = Collection::load(&f.file).unwrap();
    outside.remove(&[1]);
    outside.remove(&[0, 1]);
    outside.save().unwrap();
    f.window
        .update(cx, |view, _, cx| {
            assert!(view.refresh_from_disk(cx));
            assert!(view.tab(clean).is_none());
            assert!(view.tab(busy).is_some(), "its edits are not thrown away");
        })
        .unwrap();
}

#[gpui::test]
fn the_tree_is_built_once_until_the_collection_changes(cx: &mut TestAppContext) {
    let f = open(cx);
    f.window
        .update(cx, |view, _, cx| {
            let first = view.collections[0].entries();
            assert!(Rc::ptr_eq(&first, &view.collections[0].entries()));
            let file = view.collections[0].file.clone();
            view.collection_mut(&file)
                .unwrap()
                .collection
                .add_request(&[], "New");
            let second = view.collections[0].entries();
            assert!(!Rc::ptr_eq(&first, &second));
            assert_eq!(second.len(), first.len() + 1);
            let _ = cx;
        })
        .unwrap();
}

// ---- from the rest of Monocode ----

#[gpui::test]
fn a_curl_command_from_a_reply_becomes_an_open_request(cx: &mut TestAppContext) {
    let f = open(cx);
    f.window
        .update(cx, |view, window, cx| {
            view.import_curl_request(
                "curl -X POST https://api.test/v1/items -H 'Content-Type: application/json' -d '{\"a\":1}'",
                window,
                cx,
            );
            assert_eq!(view.tabs.len(), 1);
            let tab = view.active_tab().unwrap();
            assert_eq!(tab.draft.method, "POST");
            assert_eq!(tab.draft.name, "POST items");
            assert!(!tab.dirty(), "it is saved in the collection already");
            assert!(view.notice.as_deref().is_some_and(|n| n.contains("Added")));
            view.import_curl_request("wget nothing", window, cx);
            assert!(view.notice.as_deref().is_some_and(|n| n.contains("curl")));
            assert_eq!(view.tabs.len(), 1);
        })
        .unwrap();
    let saved = Collection::load(&f.file).unwrap();
    assert_eq!(saved.request_count(), 4);
}

#[gpui::test]
fn a_project_without_a_collection_gets_one_for_the_first_curl_command(cx: &mut TestAppContext) {
    let f = open(cx);
    std::fs::remove_file(&f.file).unwrap();
    f.window
        .update(cx, |view, window, cx| {
            view.reload(cx);
            assert!(view.collections.is_empty());
            view.import_curl_request("curl https://x.test/ping", window, cx);
            assert_eq!(view.collections.len(), 1);
            assert_eq!(view.tabs.len(), 1);
            assert!(
                view.collections[0]
                    .file
                    .ends_with("api.postman_collection.json")
            );
        })
        .unwrap();
}

#[gpui::test]
fn what_an_agent_is_asked_hides_credentials_and_includes_the_answer(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![1]);
    f.window
        .update(cx, |view, _, cx| {
            let tab = view.tab_mut(id).unwrap();
            tab.draft
                .headers
                .push(KeyValue::new("Authorization", "Bearer SECRET-ONE"));
            tab.draft
                .headers
                .push(KeyValue::new("X-Api-Key", "SECRET-TWO"));
            tab.draft
                .headers
                .push(KeyValue::new("Accept", "application/json"));
            tab.draft.auth = Auth::Bearer {
                token: "SECRET-THREE".into(),
            };
            tab.draft.body = Body::Raw {
                text: "{\"a\":1}".into(),
                language: RawLanguage::Json,
            };
            tab.generation = 1;
            tab.tests = vec![api::TestResult {
                name: "status is 200".into(),
                passed: false,
                error: Some("expected 500 to equal 200".into()),
                skipped: false,
            }];
            let output = api::ExchangeOutput {
                outcome: Ok(api::Response {
                    status: 500,
                    reason: "Server Error".into(),
                    headers: Vec::new(),
                    body: b"{\"error\":\"boom\"}".to_vec(),
                    truncated: false,
                    elapsed: Duration::from_millis(40),
                    url: String::new(),
                }),
                request: ApiRequest::default(),
                prepared: None,
                tests: tab.tests.clone(),
                logs: Vec::new(),
                environment: Vec::new(),
                collection: Vec::new(),
                globals: Vec::new(),
            };
            view.finish(id, 1, Some(output), cx);
            let prompt = view.agent_prompt(view.tab(id).unwrap());
            assert!(!prompt.contains("SECRET"), "{prompt}");
            assert!(prompt.contains("Authorization: [hidden]"));
            assert!(prompt.contains("X-Api-Key: [hidden]"));
            assert!(prompt.contains("Accept: application/json"));
            assert!(prompt.contains("POST {{baseUrl}}/login"));
            assert!(prompt.contains("Response: 500 Server Error"));
            assert!(prompt.contains("\"error\": \"boom\""));
            assert!(
                prompt.contains("Failing tests:") && prompt.contains("expected 500 to equal 200")
            );
            assert!(prompt.contains("auth: bearer token"));
        })
        .unwrap();
}

#[gpui::test]
fn asking_an_agent_says_which_project_and_what_to_open(cx: &mut TestAppContext) {
    let f = open(cx);
    let id = open_first(&f, cx, vec![0, 0]);
    let seen: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let sink = seen.clone();
    f.window
        .update(cx, |_, _, cx| {
            cx.subscribe_self(move |_, event: &Event, _| {
                let Event::AskAgent { project, title, .. } = event;
                sink.borrow_mut().push((project.clone(), title.clone()));
            })
            .detach();
        })
        .unwrap();
    f.window
        .update(cx, |view, _, cx| view.ask_agent(id, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        seen.borrow().as_slice(),
        [(f.project.clone(), "API: GET List Products".to_owned())]
    );
}

#[gpui::test]
fn every_folder_can_be_collapsed_and_expanded_at_once(cx: &mut TestAppContext) {
    let f = open(cx);
    f.window
        .update(cx, |view, _, cx| {
            let before = names(view, cx);
            assert!(before.contains(&"    List Products".to_owned()));
            view.collapse_all(cx);
            assert_eq!(names(view, cx), ["Shop", "  Products", "  Login"]);
            view.expand_all(cx);
            assert_eq!(names(view, cx), before);
        })
        .unwrap();
    cx.refresh().unwrap();
}
