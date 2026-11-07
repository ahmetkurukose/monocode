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
