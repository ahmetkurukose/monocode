use super::*;
use gpui::{TestAppContext, WindowHandle};
use monocode_core::db::ResultColumn;

fn open(cx: &mut TestAppContext, dir: &std::path::Path) -> WindowHandle<DatabaseView> {
    cx.update(input::bind);
    let dir = dir.to_path_buf();
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), move |_, cx| {
            cx.new(|cx| DatabaseView::new(dir, cx))
        })
        .unwrap()
    });
    cx.refresh().unwrap();
    window
}

fn local_source(name: &str) -> DataSource {
    let mut source = DataSource::new(name);
    source.user = "postgres".into();
    source
}

fn sample() -> QueryResult {
    QueryResult {
        statement: "select".into(),
        columns: ["id", "payload"]
            .iter()
            .map(|c| ResultColumn { name: (*c).into() })
            .collect(),
        rows: vec![
            vec![Some("1".into()), Some("{\"a\":[1,2]}".into())],
            vec![Some("2".into()), None],
        ],
        affected: Some(2),
        has_more: false,
        execution_ms: 1.,
        elapsed_ms: 2.,
    }
}

#[gpui::test]
fn a_new_source_gets_a_console_that_survives_a_restart(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    let id = window
        .update(cx, |view, _, cx| {
            assert!(view.sources.is_empty() && view.consoles.is_empty());
            view.save_source(local_source("prod"), None, cx);
            assert_eq!(view.consoles.len(), 1);
            let console = &view.consoles[0];
            console.editor.update(cx, |input, cx| {
                input.set_text("-- orders\nselect 1".into(), cx)
            });
            view.selected.clone().unwrap()
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    cx.refresh().unwrap();
    drop(window);

    let again = open(cx, dir.path());
    again
        .update(cx, |view, _, cx| {
            assert_eq!(view.sources.len(), 1);
            assert_eq!(view.selected.as_deref(), Some(id.as_str()));
            assert_eq!(view.consoles.len(), 1);
            let text = view.consoles[0].editor.read(cx).content.to_string();
            assert_eq!(text, "-- orders\nselect 1");
            assert_eq!(view.tab_title(&view.consoles[0], cx), "orders");
        })
        .unwrap();
}

#[gpui::test]
fn consoles_open_close_and_one_always_remains(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, window, cx| {
            view.save_source(local_source("a"), None, cx);
            let source = view.selected.clone().unwrap();
            view.new_console(window, cx);
            view.new_console(window, cx);
            assert_eq!(view.consoles.len(), 3);
            assert_eq!(view.active.get(&source), Some(&3));
            view.close_console(&source, 3, window, cx);
            assert_eq!(view.active.get(&source), Some(&1));
            view.close_console(&source, 1, window, cx);
            view.close_console(&source, 2, window, cx);
            assert_eq!(
                view.consoles.len(),
                1,
                "closing the last one opens a fresh console"
            );
            assert!(view.active_console().is_some());
        })
        .unwrap();
}

#[gpui::test]
fn removing_a_source_removes_its_consoles_and_picks_another(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            let a = view.selected.clone().unwrap();
            view.save_source(local_source("b"), None, cx);
            let b = view.selected.clone().unwrap();
            assert_ne!(a, b);
            view.remove_source(&b, cx);
            assert_eq!(view.selected.as_deref(), Some(a.as_str()));
            assert!(view.consoles.iter().all(|c| c.source == a));
            assert!(view.console_store.list(&b).is_empty());
            assert_eq!(DataSourceStore::new(dir.path()).load().len(), 1);
        })
        .unwrap();
}

#[gpui::test]
fn the_grid_renders_selects_copies_and_previews_json(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            let result = sample();
            let console = &mut view.consoles[0];
            console.widths = grid::column_widths(&result);
            console.numeric = grid::numeric_columns(&result);
            console.result = Some(Arc::new(result));
            console.selected = Some((0, 1));
        })
        .unwrap();
    cx.refresh().unwrap();
    window
        .update(cx, |view, window, cx| {
            view.grid_key(&key("c", false), cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
                Some("{\"a\":[1,2]}"),
                "copy takes the whole value"
            );
            view.grid_key(&key("c", true), cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
                Some("1\t{\"a\":[1,2]}")
            );
            view.grid_key(&key("down", false), cx);
            assert_eq!(view.consoles[0].selected, Some((1, 1)));
            view.grid_key(&key("down", false), cx);
            assert_eq!(
                view.consoles[0].selected,
                Some((1, 1)),
                "stops at the last row"
            );
            view.grid_key(&key("space", false), cx);
            assert_eq!(view.preview.as_ref().map(|p| p.1.as_str()), Some("NULL"));
            view.consoles[0].selected = Some((0, 1));
            view.grid_key(&key("space", false), cx);
            assert!(view.preview.as_ref().unwrap().1.contains("\n  \"a\": ["));
            let _ = window;
        })
        .unwrap();
    cx.refresh().unwrap();
}

fn key(name: &str, shift: bool) -> gpui::KeyDownEvent {
    gpui::KeyDownEvent {
        keystroke: gpui::Keystroke {
            modifiers: gpui::Modifiers {
                platform: name == "c",
                shift,
                ..Default::default()
            },
            key: name.into(),
            key_char: None,
        },
        is_held: false,
        prefer_character_input: false,
    }
}

fn long_result() -> QueryResult {
    let mut result = sample();
    result.columns = (0..30)
        .map(|i| ResultColumn {
            name: format!("column_{i}"),
        })
        .collect();
    result.rows = (0..500)
        .map(|r| {
            (0..30)
                .map(|c| Some(format!("value {r} {c} with some text")))
                .collect()
        })
        .collect();
    result
}

fn load(view: &mut DatabaseView, result: QueryResult) {
    let console = &mut view.consoles[0];
    console.widths = grid::column_widths(&result);
    console.numeric = grid::numeric_columns(&result);
    console.result = Some(Arc::new(result));
}

fn wheel(
    visual: &mut gpui::VisualTestContext,
    at: gpui::Point<gpui::Pixels>,
    dx: f32,
    dy: f32,
    shift: bool,
) {
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(dx), px(dy))),
        modifiers: gpui::Modifiers {
            shift,
            ..Default::default()
        },
        touch_phase: gpui::TouchPhase::Moved,
    });
}

#[gpui::test]
fn the_grid_scrolls_both_ways_and_stops_at_its_edges(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            load(view, long_result());
        })
        .unwrap();
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let grid = visual.debug_bounds("database-grid").expect("grid");
    let at = grid.center();
    let state = |visual: &mut gpui::VisualTestContext| {
        window
            .read_with(visual, |v, _| {
                (v.consoles[0].scroll_x, v.consoles[0].scroll_y)
            })
            .unwrap()
    };
    assert_eq!(state(visual), (0., 0.));
    // GPUI wheel deltas are negative when content moves up/left.
    wheel(visual, at, 0., -100., false);
    assert_eq!(state(visual), (0., 100.));
    wheel(visual, at, -150., 0., false);
    assert_eq!(state(visual), (150., 100.));
    wheel(visual, at, 0., -40., true);
    assert_eq!(
        state(visual),
        (190., 100.),
        "shift turns the wheel sideways"
    );
    wheel(visual, at, 9999., 9999., false);
    assert_eq!(state(visual), (0., 0.), "never past the start");
    wheel(visual, at, -99999., -999999., false);
    let (x, y) = state(visual);
    let (max_x, max_y) = window
        .read_with(visual, |v, _| v.grid_max_scroll(0))
        .unwrap();
    assert_eq!((x, y), (max_x, max_y));
    assert!(max_x > 1000. && max_y > 5000.);
    // The last row exists and the first does not once scrolled to the end.
    let rows = window
        .read_with(visual, |v, _| {
            let body = v.grid_viewport.get().1 - grid::HEADER_HEIGHT - grid::BAR;
            grid::visible_rows(v.consoles[0].scroll_y, body, 500)
        })
        .unwrap();
    assert_eq!(rows.end, 500);
    assert!(rows.start > 400);
}

#[gpui::test]
fn a_closed_console_can_be_opened_again_from_the_tree(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, window, cx| {
            view.save_source(local_source("a"), None, cx);
            let source = view.selected.clone().unwrap();
            view.consoles[0].editor.update(cx, |input, cx| {
                input.set_text("-- orders\nselect 1".into(), cx)
            });
            view.new_console(window, cx);
            view.close_console(&source, 1, window, cx);
            assert!(view.console_index(&source, 1).is_none(), "the tab is gone");
            assert!(
                view.saved[&source]
                    .iter()
                    .any(|(id, title)| *id == 1 && title == "orders"),
                "but the console is still listed"
            );
            view.expanded.insert(format!("s:{source}"));
            view.expanded.insert(format!("c:{source}"));
            let rows = view.rows();
            assert!(
                rows.iter()
                    .any(|r| matches!(r, TreeRow::SavedConsole(_, 1, t, false) if t == "orders"))
            );
            view.open_console(&source, 1, window, cx);
            assert_eq!(view.active.get(&source), Some(&1));
            let text = view
                .active_console()
                .unwrap()
                .editor
                .read(cx)
                .content
                .to_string();
            assert_eq!(text, "-- orders\nselect 1");
            view.delete_console(&source, 1, window, cx);
            assert!(!view.saved[&source].iter().any(|(id, _)| *id == 1));
            assert!(view.console_store.list(&source).iter().all(|id| *id != 1));
        })
        .unwrap();
}

#[gpui::test]
fn open_tabs_come_back_after_a_restart_and_closed_ones_stay_closed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    let source = window
        .update(cx, |view, window, cx| {
            view.save_source(local_source("a"), None, cx);
            let source = view.selected.clone().unwrap();
            view.consoles[0]
                .editor
                .update(cx, |i, cx| i.set_text("select 1".into(), cx));
            view.new_console(window, cx);
            view.consoles[1]
                .editor
                .update(cx, |i, cx| i.set_text("select 2".into(), cx));
            view.close_console(&source, 1, window, cx);
            view.save_state();
            source
        })
        .unwrap();
    drop(window);
    let again = open(cx, dir.path());
    again
        .update(cx, |view, _, _| {
            let open: Vec<u32> = view.consoles.iter().map(|c| c.id).collect();
            assert_eq!(open, [2]);
            assert_eq!(view.saved[&source].len(), 2, "console 1 is still saved");
        })
        .unwrap();
}

#[gpui::test]
fn the_editor_is_coloured_as_sql_and_disconnect_forgets_the_tree(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            let source = view.selected.clone().unwrap();
            view.consoles[0].editor.update(cx, |i, cx| {
                i.replace_bytes(0..0, "select 'x' from t", 17, cx)
            });
            view.expanded.insert(format!("s:{source}"));
            view.nodes.insert(source.clone(), SourceNode::new());
            view.disconnect_source(&source, cx);
            assert!(!view.nodes.contains_key(&source));
            assert!(!view.expanded.contains(&format!("s:{source}")));
        })
        .unwrap();
    // The editor's change event is delivered once the update above has ended.
    window
        .update(cx, |view, _, cx| {
            let tokens = view.consoles[0]
                .editor
                .read(cx)
                .editor_analysis()
                .tokens
                .len();
            assert!(
                tokens >= 3,
                "keywords and the string are coloured, got {tokens}"
            );
        })
        .unwrap();
}

#[gpui::test]
fn the_loaded_rows_copy_as_csv_and_json(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            load(view, sample());
            view.copy_result(monocode_core::db::ExportFormat::Csv, cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
                Some("id,payload\n1,\"{\"\"a\"\":[1,2]}\"\n2,\n")
            );
            view.copy_result(monocode_core::db::ExportFormat::Json, cx);
            let text = cx.read_from_clipboard().and_then(|c| c.text()).unwrap();
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(json[0]["payload"]["a"][1], 2);
            assert!(json[1]["payload"].is_null());
            assert_eq!(
                view.consoles[0].note.as_deref(),
                Some("Copied 2 rows as JSON")
            );
        })
        .unwrap();
}

#[gpui::test]
fn typing_offers_tables_and_columns_from_the_catalog(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            let source = view.selected.clone().unwrap();
            *view.catalog_for(&source).write().unwrap() =
                sql::Catalog::from_entries(vec![monocode_core::db::CatalogEntry {
                    schema: "public".into(),
                    table: "orders".into(),
                    kind: TableKind::Table,
                    column: "customer_id".into(),
                    type_name: "integer".into(),
                }]);
            view.consoles[0].editor.update(cx, |i, cx| {
                i.replace_bytes(0..0, "select * from ord", 17, cx);
                i.schedule_completion(true, cx);
            });
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |view, _, cx| {
            let labels = view.consoles[0].editor.read(cx).completion_labels();
            assert_eq!(labels, ["orders"]);
        })
        .unwrap();
}

#[test]
fn table_pages_and_sorting_build_the_expected_sql() {
    let mut paging = Paging {
        schema: "public".into(),
        table: "Order Items".into(),
        page: 2,
        sort: None,
    };
    assert_eq!(
        page_sql(&paging, 500),
        "select * from \"public\".\"Order Items\" limit 500 offset 1000"
    );
    paging.sort = Some(("total\"x".into(), true));
    assert_eq!(
        page_sql(&paging, 100),
        "select * from \"public\".\"Order Items\" order by \"total\"\"x\" desc limit 100 offset 200"
    );
    assert_eq!(
        table_sql(&paging),
        "select * from \"public\".\"Order Items\" order by \"total\"\"x\" desc",
        "exporting a table ignores the page"
    );
    let a = next_sort(None, "a".into());
    assert_eq!(a, Some(("a".into(), false)));
    let a = next_sort(a, "a".into());
    assert_eq!(a, Some(("a".into(), true)));
    assert_eq!(next_sort(a.clone(), "a".into()), None);
    assert_eq!(next_sort(a, "b".into()), Some(("b".into(), false)));
}

#[gpui::test]
fn history_statements_go_into_the_console_without_losing_what_is_there(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, window, cx| {
            view.save_source(local_source("a"), None, cx);
            view.history_open = true;
            view.insert_from_history("select 1", false, window, cx);
            assert!(!view.history_open);
            let text = view.consoles[0].editor.read(cx).content.to_string();
            assert_eq!(text, "select 1", "an empty console takes it as is");
            view.insert_from_history("select 2", false, window, cx);
            let text = view.consoles[0].editor.read(cx).content.to_string();
            assert_eq!(text, "select 1\n\nselect 2", "otherwise it is appended");
        })
        .unwrap();
}
