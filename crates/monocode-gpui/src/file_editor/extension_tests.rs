//! The file pane's side of the extension points: what a document's
//! subscribers hear and in what order, how hub state reaches the editor,
//! and the history and header built on it.
use super::*;
use crate::code_editor::{
    self, DocumentEvent, DocumentInfo, ExternalDiagnostic, GutterMark, GutterMarkKind, GutterSlot,
    HeaderAction, HeaderProvider, LinkProvider, LinkTarget, Position, Severity, TargetPosition,
};
use gpui::{
    Bounds, EntityInputHandler, TestAppContext, VisualTestContext, WindowBounds, WindowHandle,
    WindowOptions, point, size,
};
use std::{cell::RefCell, path::Path, rc::Rc};

fn open(path: &Path, cx: &mut TestAppContext) -> WindowHandle<FileEditor> {
    let file = path.to_string_lossy().into_owned();
    let root = path.parent().unwrap().to_string_lossy().into_owned();
    cx.update(|cx| {
        input::bind(cx);
        bind(cx);
        crate::popover::bind(cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(700.), px(400.)),
                ))),
                ..Default::default()
            },
            |w, cx| {
                let git = cx.new(|cx| ProjectGit::new(root, cx));
                let view = cx.new(|cx| FileEditor::new(file, git, cx));
                w.focus(&view.focus_handle(cx), cx);
                view
            },
        )
        .unwrap()
    })
}
fn settle(cx: &mut VisualTestContext, millis: u64) {
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(millis));
    cx.run_until_parked();
}
fn type_text(window: WindowHandle<FileEditor>, text: &str, cx: &mut VisualTestContext) {
    window
        .update(cx, |view, w, cx| {
            view.input.update(cx, |input, cx| {
                input.replace_text_in_range(None, text, w, cx)
            })
        })
        .unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn a_document_reports_open_coalesced_changes_save_and_close_in_order(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let events: Rc<RefCell<Vec<String>>> = Rc::default();
    let subscription = cx.update(|cx| {
        let events = events.clone();
        let hub = code_editor::hub(cx);
        cx.subscribe(&hub, move |_, event: &DocumentEvent, _| {
            events.borrow_mut().push(match event {
                DocumentEvent::Opened { doc, text, version } => {
                    assert_eq!(doc.language, Some("python"));
                    format!("open v{version} {text:?}")
                }
                DocumentEvent::Changed { text, version, .. } => {
                    format!("change v{version} {text:?}")
                }
                DocumentEvent::Saved { text, version, .. } => format!("save v{version} {text:?}"),
                DocumentEvent::Closed { doc } => {
                    format!("close {}", doc.path.file_name().unwrap().to_string_lossy())
                }
            });
        })
    });
    let window = open(&path, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    assert_eq!(*events.borrow(), ["open v1 \"value = 1\\n\""]);
    window
        .update(cx, |view, _, cx| {
            assert_eq!(view.extensions.doc.path, path);
            assert_eq!(view.extensions.doc.root, dir.path());
            let hub = code_editor::hub(cx);
            assert_eq!(hub.read(cx).open_documents().count(), 1);
        })
        .unwrap();
    // A burst of typing is one change, sent when it pauses.
    for typed in ["#", " ", "x"] {
        type_text(window, typed, cx);
        cx.executor().advance_clock(Duration::from_millis(50));
    }
    cx.run_until_parked();
    assert_eq!(events.borrow().len(), 1, "nothing is sent mid-burst");
    settle(cx, 250);
    assert_eq!(events.borrow()[1], "change v2 \"# xvalue = 1\\n\"");
    // A save flushes what is pending first, so the saved text is known.
    type_text(window, "!", cx);
    window.update(cx, |view, _, cx| view.save(cx)).unwrap();
    settle(cx, 250);
    assert_eq!(
        events.borrow()[2..],
        [
            "change v3 \"# x!value = 1\\n\"".to_owned(),
            "save v3 \"# x!value = 1\\n\"".to_owned()
        ]
    );
    // A provider request brings subscribers up to date without waiting.
    type_text(window, "?", cx);
    window
        .update(cx, |view, _, cx| assert_eq!(view.sync_document(cx), 4))
        .unwrap();
    assert_eq!(events.borrow()[4], "change v4 \"# x!?value = 1\\n\"");
    settle(cx, 250);
    assert_eq!(events.borrow().len(), 5, "the pending timer adds nothing");
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(events.borrow().last().unwrap(), "close app.py");
    // The window is gone, so this asks the application itself.
    cx.cx.update(|cx| {
        assert_eq!(code_editor::hub(cx).read(cx).open_documents().count(), 0);
    });
    drop(subscription);
}

#[gpui::test]
fn hub_diagnostics_and_marks_reach_the_editor_at_open_and_after_at_their_version(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.py");
    let text = "import os\nvalue = missing\n";
    std::fs::write(&path, text).unwrap();
    let missing = text.find("missing").unwrap();
    let error = |range: std::ops::Range<usize>| {
        ExternalDiagnostic::new(range, Severity::Error, "not defined", "basedpyright")
    };
    // Known before the file is open: a project-wide check, a breakpoint.
    cx.update(|cx| {
        code_editor::hub(cx).update(cx, |hub, cx| {
            hub.set_diagnostics(
                &path,
                "ruff",
                None,
                vec![ExternalDiagnostic::new(
                    7..9,
                    Severity::Warning,
                    "unused import",
                    "ruff",
                )],
                cx,
            );
            hub.set_gutter_marks(
                &path,
                GutterSlot::Breakpoint,
                vec![GutterMark {
                    row: 1,
                    kind: GutterMarkKind::Breakpoint(code_editor::Breakpoint::Enabled),
                    tooltip: "Breakpoint".into(),
                }],
                cx,
            );
            assert_eq!(hub.diagnostic_counts(&path).warnings, 1);
        });
    });
    let window = open(&path, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let ranges = |cx: &mut VisualTestContext| {
        window
            .update(cx, |view, _, cx| {
                let input = view.input.read(cx);
                input
                    .external_diagnostics()
                    .iter()
                    .map(|d| input.content[d.range.clone()].to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap()
    };
    assert_eq!(ranges(cx), ["os"]);
    window
        .update(cx, |view, _, cx| {
            let input = view.input.read(cx);
            assert_eq!(input.gutter_marks(GutterSlot::Breakpoint)[0].row, 1);
            assert_eq!(input.diagnostic_counts().warnings, 1);
        })
        .unwrap();
    // The server answers for version 1 while a newer edit is still unsent:
    // its ranges are carried over that edit.
    window
        .update(cx, |view, _, cx| {
            view.input
                .update(cx, |input, cx| input.select_range(0..0, cx))
        })
        .unwrap();
    type_text(window, "# top\n", cx);
    cx.update(|_, cx| {
        code_editor::hub(cx).update(cx, |hub, cx| {
            hub.set_diagnostics(
                &path,
                "basedpyright",
                Some(1),
                vec![error(missing..missing + 7)],
                cx,
            )
        });
    });
    cx.run_until_parked();
    assert_eq!(ranges(cx), ["os", "missing"]);
    window
        .update(cx, |view, _, cx| {
            let input = view.input.read(cx);
            assert_eq!(input.gutter_marks(GutterSlot::Breakpoint)[0].row, 2);
            assert_eq!(input.diagnostic_counts().errors, 1);
            assert_eq!(view.header_parts(cx).trailing.len(), 1, "the problem count");
        })
        .unwrap();
    // Once version 2 is out, an answer for version 1 is too old to place.
    settle(cx, 250);
    cx.update(|_, cx| {
        code_editor::hub(cx).update(cx, |hub, cx| {
            assert_eq!(hub.document(&path).unwrap().version, 2);
            hub.set_diagnostics(&path, "basedpyright", Some(1), Vec::new(), cx);
            assert_eq!(hub.diagnostics(&path, Some("basedpyright")).len(), 1);
            hub.set_diagnostics(&path, "basedpyright", Some(2), Vec::new(), cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(ranges(cx), ["os"]);
}

#[gpui::test]
fn one_columns_new_marks_do_not_bring_back_the_other_columns_old_rows(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_app.py");
    std::fs::write(&path, "def test_a():\n    pass\n").unwrap();
    let mark = |row, kind| GutterMark {
        row,
        kind,
        tooltip: "mark".into(),
    };
    let breakpoint = |row| {
        mark(
            row,
            GutterMarkKind::Breakpoint(code_editor::Breakpoint::Enabled),
        )
    };
    cx.update(|cx| {
        code_editor::hub(cx).update(cx, |hub, cx| {
            hub.set_gutter_marks(&path, GutterSlot::Breakpoint, vec![breakpoint(1)], cx);
            hub.set_gutter_marks(
                &path,
                GutterSlot::Run,
                vec![mark(0, GutterMarkKind::Run)],
                cx,
            );
        });
    });
    let window = open(&path, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    window
        .update(cx, |view, _, cx| {
            view.input
                .update(cx, |input, cx| input.select_range(0..0, cx))
        })
        .unwrap();
    type_text(window, "import os\n", cx);
    let rows = |cx: &mut VisualTestContext, slot| {
        window
            .update(cx, |view, _, cx| {
                let input = view.input.read(cx);
                (input.gutter_marks(slot).iter().map(|mark| mark.row)).collect::<Vec<_>>()
            })
            .unwrap()
    };
    // The editor moved both columns itself.
    assert_eq!(rows(cx, GutterSlot::Breakpoint), [2]);
    assert_eq!(rows(cx, GutterSlot::Run), [1]);
    // The breakpoint column reports the move; the hub still holds the run
    // marks of before the edit and says nothing about them.
    cx.update(|_, cx| {
        code_editor::hub(cx).update(cx, |hub, cx| {
            hub.set_gutter_marks(&path, GutterSlot::Breakpoint, vec![breakpoint(2)], cx)
        });
    });
    cx.run_until_parked();
    assert_eq!(rows(cx, GutterSlot::Breakpoint), [2]);
    assert_eq!(rows(cx, GutterSlot::Run), [1]);
}

#[gpui::test]
fn two_editors_on_one_file_follow_the_one_edited_last(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let hub_text = |cx: &mut VisualTestContext, path: &Path| {
        cx.update(|_, cx| {
            let hub = code_editor::hub(cx);
            let open = hub
                .read(cx)
                .document(path)
                .map(|open| open.text.to_string());
            (
                open,
                hub.read(cx).document(path).map_or(0, |open| open.version),
            )
        })
    };
    let first = open(&path, cx);
    let first_cx = &mut VisualTestContext::from_window(first.into(), cx);
    first_cx.run_until_parked();
    first
        .update(first_cx, |view, _, cx| {
            view.input
                .update(cx, |input, cx| input.select_range(0..0, cx))
        })
        .unwrap();
    type_text(first, "# typed in the first\n", first_cx);
    settle(first_cx, 250);
    let typed = "# typed in the first\nvalue = 1\n";
    assert_eq!(hub_text(first_cx, &path).0.as_deref(), Some(typed));

    // A second pane opens the file from disk: that is not an edit.
    let second = open(&path, cx);
    let second_cx = &mut VisualTestContext::from_window(second.into(), cx);
    second_cx.run_until_parked();
    let (text, version) = hub_text(second_cx, &path);
    assert_eq!(text.as_deref(), Some(typed));
    // Nor is it when the second one is asked to report what it has not
    // changed, as a hover or a completion does.
    let reported = second
        .update(second_cx, |view, _, cx| view.sync_document(cx))
        .unwrap();
    assert_eq!(reported, version);
    assert_eq!(
        hub_text(second_cx, &path),
        (Some(typed.to_owned()), version)
    );

    // Editing in the second one makes it the one the document follows, and
    // the first one's unchanged buffer no longer disturbs it.
    second
        .update(second_cx, |view, _, cx| {
            view.input
                .update(cx, |input, cx| input.select_range(0..0, cx))
        })
        .unwrap();
    type_text(second, "# typed in the second\n", second_cx);
    settle(second_cx, 250);
    let newest = "# typed in the second\nvalue = 1\n";
    assert_eq!(hub_text(second_cx, &path).0.as_deref(), Some(newest));
    first
        .update(second_cx, |view, _, cx| view.sync_document(cx))
        .unwrap();
    assert_eq!(hub_text(second_cx, &path).0.as_deref(), Some(newest));
}

struct Feature {
    lib: PathBuf,
}
impl LinkProvider for Feature {
    fn targets(&self, _: Position, _: &mut App) -> Task<Vec<LinkTarget>> {
        Task::ready(vec![LinkTarget {
            path: self.lib.clone(),
            position: TargetPosition::LineColumn { line: 2, column: 5 },
        }])
    }
}
impl HeaderProvider for Feature {
    fn actions(&self, _: &DocumentInfo, _: &App) -> Vec<HeaderAction> {
        vec![HeaderAction::new(
            "run-file",
            "play",
            "Run file",
            |_, _, _| {},
        )]
    }
}

#[gpui::test]
fn a_registered_feature_attaches_itself_and_jumps_walk_back_and_forward_across_files(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app.py");
    let lib = dir.path().join("lib.py");
    let source =
        "from lib import area\n\nclass Shape:\n    def area(self):\n        return area(self)\n";
    std::fs::write(&app, source).unwrap();
    std::fs::write(&lib, "import math\ndef area(shape):\n    return 0\n").unwrap();
    // Stands in for the shell, which opens the file an `OpenPath` names.
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    cx.update(|cx| {
        let opened = opened.clone();
        cx.on_action(move |action: &OpenPath, _| opened.borrow_mut().push(action.0.clone()));
        let lib = lib.clone();
        code_editor::register(cx, move |doc, providers, _| {
            if doc.language == Some("python") {
                let feature = Rc::new(Feature { lib: lib.clone() });
                providers.links = Some(feature.clone());
                providers.header.push(feature);
            }
        });
    });
    let window = open(&app, cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    settle(cx, 200);
    let call = source.rfind("area").unwrap();
    window
        .update(cx, |view, _, cx| {
            assert!(
                view.input
                    .read(cx)
                    .editor_providers()
                    .is_some_and(|providers| providers.links.is_some()),
                "the feature never touched the file pane"
            );
            // Nothing to go back to yet: one button, no arrows, no crumbs.
            let parts = view.header_parts(cx);
            assert!(parts.leading.is_none() && parts.crumbs.is_empty());
            assert_eq!(parts.trailing.len(), 1);
            // The outline names where the caret is.
            view.input
                .update(cx, |input, cx| input.select_range(call + 1..call + 1, cx));
            let crumbs: Vec<_> = view.crumbs(cx).into_iter().map(|c| c.label).collect();
            assert_eq!(crumbs, ["Shape", "area"]);
            assert_eq!(view.header_parts(cx).crumbs.len(), 4, "two with separators");
            assert_eq!(view.outline().len(), 2);
        })
        .unwrap();
    // Go to definition lands in another file, through the file route.
    cx.simulate_keystrokes("f12");
    let lib_link = format!("{}:2:5", lib.display());
    assert_eq!(*opened.borrow(), [lib_link.clone()]);
    window
        .update(cx, |view, w, cx| {
            assert!(view.header_parts(cx).leading.is_some(), "the arrows appear");
            // Back from this pane's point of view is within its own file.
            view.input
                .update(cx, |input, cx| input.select_range(0..0, cx));
            let hub = code_editor::hub(cx);
            assert!(hub.read(cx).history().can_back());
            // The other file's pane goes back: here, to line 5.
            let back = hub.update(cx, |hub, cx| hub.back(None, cx)).unwrap();
            assert_eq!((back.path.clone(), back.line), (app.clone(), 5));
            view.navigate(back.line, back.column, cx);
            assert_eq!(view.input.read(cx).caret(), call + 1);
            // Forward from here reopens the other file.
            view.navigate_history(false, w, cx);
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(*opened.borrow(), [lib_link.clone(), lib_link]);
    // A crumb is a jump inside the file, undone with the Back key.
    window
        .update(cx, |view, w, cx| {
            let class = source.find("Shape").unwrap();
            view.jump_to(class, w, cx);
            assert_eq!(view.input.read(cx).caret(), class);
        })
        .unwrap();
    cx.simulate_keystrokes("ctrl--");
    window
        .update(cx, |view, _, cx| {
            assert_eq!(view.input.read(cx).caret(), call + 1);
        })
        .unwrap();
}
