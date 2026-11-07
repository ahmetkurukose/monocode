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

fn details() -> TableDetails {
    use monocode_core::db::{ConstraintInfo, IndexInfo, TriggerInfo};
    TableDetails {
        indexes: vec![
            IndexInfo {
                name: "orders_pkey".into(),
                definition: "CREATE UNIQUE INDEX orders_pkey ON public.orders USING btree (id)"
                    .into(),
                unique: true,
                primary: true,
            },
            IndexInfo {
                name: "orders_customer_idx".into(),
                definition:
                    "CREATE INDEX orders_customer_idx ON public.orders USING btree (customer_id)"
                        .into(),
                unique: false,
                primary: false,
            },
        ],
        constraints: vec![
            ConstraintInfo {
                name: "orders_pkey".into(),
                kind: 'p',
                definition: "PRIMARY KEY (id)".into(),
            },
            ConstraintInfo {
                name: "orders_customer_fk".into(),
                kind: 'f',
                definition: "FOREIGN KEY (customer_id) REFERENCES customers(id)".into(),
            },
        ],
        referenced_by: vec![(
            "order_items".into(),
            "item_order_fk FOREIGN KEY (order_id) REFERENCES orders(id)".into(),
        )],
        triggers: Vec::new(),
    }
}

#[test]
fn a_tables_groups_list_indexes_foreign_keys_and_what_references_it() {
    let columns = vec![ColumnInfo {
        name: "id".into(),
        type_name: "integer".into(),
        nullable: false,
        default: None,
        primary_key: true,
    }];
    let details = details();
    let leaf = |kind| group_rows(&columns, Some(&details), kind);
    let TreeRow::Leaf {
        label,
        detail,
        full,
        ..
    } = &leaf(GroupKind::Indexes)[0]
    else {
        panic!("an index row");
    };
    assert_eq!(label, "orders_pkey");
    assert_eq!(detail, "primary · btree (id)");
    assert!(full.starts_with("CREATE UNIQUE INDEX"));
    let TreeRow::Leaf { detail, .. } = &leaf(GroupKind::Indexes)[1] else {
        panic!();
    };
    assert_eq!(detail, "btree (customer_id)");
    let fks = leaf(GroupKind::ForeignKeys);
    assert_eq!(fks.len(), 1);
    let TreeRow::Leaf { detail, .. } = &fks[0] else {
        panic!()
    };
    assert_eq!(detail, "(customer_id) REFERENCES customers(id)");
    assert_eq!(
        leaf(GroupKind::Constraints).len(),
        1,
        "the foreign key is not repeated"
    );
    assert_eq!(leaf(GroupKind::ReferencedBy).len(), 1);
    assert!(leaf(GroupKind::Triggers).is_empty());
    assert_eq!(leaf(GroupKind::Columns).len(), 1);
    assert_eq!(group_rows(&columns, None, GroupKind::Indexes).len(), 0);
}

#[gpui::test]
fn an_expanded_table_shows_its_folders_and_hiding_results_survives_a_restart(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            let source = view.selected.clone().unwrap();
            let mut node = SourceNode::new();
            node.link = Link::Ready;
            node.schemas = Some(vec![SchemaNode {
                name: "public".into(),
                tables: Some(vec![TableNode {
                    info: TableInfo {
                        name: "orders".into(),
                        kind: TableKind::Table,
                        estimated_rows: None,
                        comment: None,
                    },
                    columns: Some(vec![ColumnInfo {
                        name: "id".into(),
                        type_name: "integer".into(),
                        nullable: false,
                        default: None,
                        primary_key: true,
                    }]),
                    details: Some(details()),
                }]),
            }]);
            view.nodes.insert(source.clone(), node);
            for key in [
                format!("s:{source}"),
                format!("n:{source}/public"),
                format!("t:{source}/public/orders"),
                group_key(&source, "public", "orders", GroupKind::Indexes),
            ] {
                view.expanded.insert(key);
            }
            let rows = view.rows();
            let groups: Vec<GroupKind> = rows
                .iter()
                .filter_map(|r| match r {
                    TreeRow::Group(_, _, _, kind) => Some(*kind),
                    _ => None,
                })
                .collect();
            assert_eq!(
                groups,
                [
                    GroupKind::Columns,
                    GroupKind::Indexes,
                    GroupKind::ForeignKeys,
                    GroupKind::Constraints,
                    GroupKind::ReferencedBy
                ],
                "empty folders (triggers) are left out"
            );
            let leaves = rows
                .iter()
                .filter(|r| matches!(r, TreeRow::Leaf { .. }))
                .count();
            assert_eq!(leaves, 2, "only the expanded folder lists its entries");
            assert!(
                !rows.iter().any(|r| matches!(r, TreeRow::Column(..))),
                "columns folder is collapsed"
            );
            view.toggle_results(cx);
            assert!(view.results_hidden);
        })
        .unwrap();
    drop(window);
    let again = open(cx, dir.path());
    again
        .update(cx, |view, _, _| assert!(view.results_hidden))
        .unwrap();
}

#[gpui::test]
fn a_script_with_several_queries_gets_a_tab_per_result(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            let console = &mut view.consoles[0];
            let mut first = sample();
            first.statement = "select 1".into();
            let mut second = long_result();
            second.statement = "select * from t".into();
            console.all = vec![Arc::new(first), Arc::new(second)];
            console.select_result(1);
            console.scroll_y = 500.;
            assert_eq!(
                console.result.as_ref().unwrap().statement,
                "select * from t"
            );
            assert_eq!(console.widths.len(), 30);
            console.select_result(0);
            assert_eq!(console.widths.len(), 2, "the grid follows the tab");
            assert_eq!(console.scroll_y, 0., "and starts at the top");
            console.select_result(9);
            assert_eq!(console.current, 0, "an unknown tab changes nothing");
        })
        .unwrap();
    cx.refresh().unwrap();
}

#[gpui::test]
fn a_console_can_be_renamed_and_the_name_sticks(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    let source = window
        .update(cx, |view, window, cx| {
            view.save_source(local_source("a"), None, cx);
            let source = view.selected.clone().unwrap();
            view.consoles[0].editor.update(cx, |i, cx| {
                i.set_text("-- from the comment\nselect 1".into(), cx)
            });
            // Typing saves the text after a short pause; do that now.
            let _ = view
                .console_store
                .save(&source, 1, "-- from the comment\nselect 1");
            assert_eq!(view.tab_title(&view.consoles[0], cx), "from the comment");
            view.open_rename(&source, 1, window, cx);
            assert!(view.rename.is_some());
            view.rename_input
                .update(cx, |i, cx| i.set_text("  Orders report ".into(), cx));
            view.commit_rename(cx);
            assert!(view.rename.is_none());
            assert_eq!(view.tab_title(&view.consoles[0], cx), "Orders report");
            assert!(
                view.saved[&source]
                    .iter()
                    .any(|(id, t)| *id == 1 && t == "Orders report")
            );
            source
        })
        .unwrap();
    drop(window);
    let again = open(cx, dir.path());
    again
        .update(cx, |view, window, cx| {
            assert_eq!(
                view.tab_title(&view.consoles[0], cx),
                "Orders report",
                "the name survives a restart"
            );
            view.open_rename(&source, 1, window, cx);
            view.rename_input
                .update(cx, |i, cx| i.set_text(String::new(), cx));
            view.commit_rename(cx);
            assert_eq!(
                view.tab_title(&view.consoles[0], cx),
                "from the comment",
                "an empty name goes back to the automatic one"
            );
        })
        .unwrap();
}

fn read_only_source(name: &str) -> DataSource {
    let mut source = local_source(name);
    source.read_only = true;
    source
}

fn type_text(view: &mut DatabaseView, text: &str, cx: &mut Context<DatabaseView>) {
    let len = text.len();
    view.consoles[0]
        .editor
        .update(cx, |i, cx| i.replace_bytes(0..0, text, len, cx));
}

#[gpui::test]
fn a_write_on_a_read_only_source_is_refused_before_it_reaches_the_server(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(read_only_source("ro"), None, cx);
            type_text(view, "delete from orders where id = 1", cx);
        })
        .unwrap();
    // The editor's change event lands after the update that typed.
    window
        .update(cx, |view, _, cx| {
            let source = view.selected.clone().unwrap();
            view.run(&source, 1, true, cx);
            assert!(!view.consoles[0].running, "nothing was sent");
            let (error, statement) = view.consoles[0].error.clone().unwrap();
            assert!(error.message.contains("read-only"), "{}", error.message);
            assert!(statement.starts_with("delete"));
            assert!(view.pending.is_none());
        })
        .unwrap();
    window
        .update(cx, |view, _, cx| {
            let analysis = view.consoles[0].editor.read(cx).editor_analysis().clone();
            assert_eq!(analysis.diagnostics.len(), 1, "the statement is underlined");
            assert!(analysis.hints[0].text.starts_with('✗') && !analysis.hints[0].ok);
        })
        .unwrap();
    // Painting the note and the underline must not panic.
    cx.refresh().unwrap();
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
}

#[gpui::test]
fn a_sweeping_write_asks_first_and_parameters_are_asked_for_and_bound(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            let mut writable = local_source("rw");
            writable.read_only = false;
            view.save_source(writable, None, cx);
            type_text(view, "update orders set total = 0", cx);
        })
        .unwrap();
    window
        .update(cx, |view, _, cx| {
            let source = view.selected.clone().unwrap();
            view.run(&source, 1, true, cx);
            assert!(matches!(&view.pending, Some(Pending::Confirm { reason, .. }) if reason.contains("every row")));
            assert!(!view.consoles[0].running, "it waits for an answer");
            view.pending = None;
        })
        .unwrap();
    window
        .update(cx, |view, _, cx| {
            let source = view.selected.clone().unwrap();
            // With parameters the dialog comes first; on a read-only source the bound
            // write is then refused without touching the network.
            view.save_source(read_only_source("ro"), None, cx);
            let ro = view.selected.clone().unwrap();
            assert_ne!(ro, source);
            let console = view.consoles.iter().position(|c| c.source == ro).unwrap();
            view.consoles[console].editor.update(cx, |i, cx| {
                i.replace_bytes(0..0, "update t set a = :v where id = $1", 33, cx)
            });
            let id = view.consoles[console].id;
            view.run(&ro, id, true, cx);
            let Some(Pending::Params { .. }) = &view.pending else {
                panic!("the parameter dialog should be open");
            };
            let names: Vec<&str> = view.param_inputs.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, [":v", "$1"]);
            view.param_inputs[0]
                .1
                .update(cx, |i, cx| i.set_text("'x'".into(), cx));
            view.param_inputs[1]
                .1
                .update(cx, |i, cx| i.set_text("7".into(), cx));
            view.submit_params(cx);
            assert!(view.pending.is_none());
            assert_eq!(
                view.param_values[":v"], "'x'",
                "values are remembered for the next run"
            );
            let (error, bound) = view.consoles[console].error.clone().unwrap();
            assert!(error.message.contains("read-only"));
            assert_eq!(
                bound, "update t set a = 'x' where id = 7",
                "the statement was bound before it was vetted"
            );
        })
        .unwrap();
}

#[gpui::test]
fn format_rewrites_the_console_in_one_undoable_step(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            type_text(view, "select a,b from t where x=1 and y=2", cx);
            view.format_console(cx);
            let text = view.consoles[0].editor.read(cx).content.to_string();
            assert_eq!(
                text,
                "select a,\n       b\nfrom t\nwhere x = 1\n  and y = 2"
            );
            view.format_console(cx);
            let again = view.consoles[0].editor.read(cx).content.to_string();
            assert_eq!(again, text, "formatting is idempotent");
        })
        .unwrap();
}

#[gpui::test]
fn the_export_dialog_names_the_table_and_its_options_shape_the_copy(cx: &mut TestAppContext) {
    use monocode_core::db::ExportFormat;
    let dir = tempfile::tempdir().unwrap();
    let window = open(cx, dir.path());
    window
        .update(cx, |view, _, cx| {
            view.save_source(local_source("a"), None, cx);
            let mut result = sample();
            result.statement = "select * from public.orders where id > 0".into();
            load(view, result);
            view.open_export(cx);
            assert!(view.export_menu);
            assert_eq!(view.export_table.read(cx).content.as_ref(), "public.orders");
            view.copy_result(ExportFormat::SqlInserts, cx);
            let text = cx.read_from_clipboard().and_then(|c| c.text()).unwrap();
            assert!(
                text.starts_with(
                    "INSERT INTO public.orders (id, payload) VALUES (1, '{\"a\":[1,2]}');"
                ),
                "{text}"
            );
            view.export_header = false;
            view.copy_result(ExportFormat::Markdown, cx);
            let md = cx.read_from_clipboard().and_then(|c| c.text()).unwrap();
            assert!(md.starts_with("|   |   |\n| --- | --- |\n| 1 |"), "{md}");
            view.export_transpose = true;
            view.export_header = true;
            view.copy_result(ExportFormat::Csv, cx);
            let csv = cx.read_from_clipboard().and_then(|c| c.text()).unwrap();
            assert_eq!(csv, "id,1,2\npayload,\"{\"\"a\"\":[1,2]}\",\n");
            // A table view names its own table.
            view.consoles[0].paging = Some(Paging {
                schema: "audit".into(),
                table: "Events".into(),
                page: 0,
                sort: None,
            });
            view.open_export(cx);
            assert_eq!(
                view.export_table.read(cx).content.as_ref(),
                "audit.\"Events\""
            );
        })
        .unwrap();
    cx.refresh().unwrap();
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
}
