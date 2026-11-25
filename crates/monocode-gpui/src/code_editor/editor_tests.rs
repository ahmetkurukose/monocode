//! Tests of the editor's extension points against fake providers: what an
//! editor asks, what it drops, and what it draws and reports back.
use super::*;
use crate::code_editor::{
    Breakpoint, CompletionProvider, CompletionRequest, CompletionTrigger, DiagnosticCounts,
    DocumentInfo, ExternalDiagnostic, GutterButton, GutterClick, GutterMark, GutterMarkKind,
    GutterProvider, GutterSlot, Hover, HoverProvider, InlineNote, LineDecoration, LineStyle,
    LinkProvider, LinkTarget, NoteTone, Position, Providers, References, ReferencesProvider,
    SLOT_WIDTH, Signature, SignatureProvider, TargetPosition, TextEdit, links,
};
use gpui::{
    Modifiers, TestAppContext, VisualTestContext, WindowBounds, WindowHandle, WindowOptions,
};
use std::{cell::RefCell, path::PathBuf, time::Duration};

fn editor(cx: &mut TestAppContext) -> WindowHandle<Input> {
    cx.update(|cx| {
        bind(cx);
        crate::popover::bind(cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(500.), px(300.)),
                ))),
                ..Default::default()
            },
            |_, cx| cx.new(Input::editor),
        )
        .unwrap()
    })
}
fn settle(cx: &mut VisualTestContext, millis: u64) {
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(millis));
    cx.run_until_parked();
}
/// A window position inside the character at `offset`.
fn glyph(i: &Input, offset: usize) -> Point<Pixels> {
    i.bounds.unwrap().origin + i.code_position(offset) - point(i.scroll_x, i.scroll_y)
        + point(px(3.), px(10.))
}

#[gpui::test]
fn outside_diagnostics_follow_edits_and_share_the_rail_with_syntax_errors(cx: &mut TestAppContext) {
    let window = editor(cx);
    let text = "import os\nvalue = undefined_name\n";
    let name = text.find("undefined_name").unwrap();
    window
        .update(cx, |i, _, cx| {
            i.set_text(text.into(), cx);
            assert!(i.code.as_ref().unwrap().decor.is_none());
            i.set_external_diagnostics(
                "basedpyright",
                vec![ExternalDiagnostic::new(
                    name..name + 14,
                    Severity::Error,
                    "\"undefined_name\" is not defined",
                    "basedpyright",
                )],
                cx,
            );
            i.set_external_diagnostics(
                "ruff",
                vec![ExternalDiagnostic::new(
                    7..9,
                    Severity::Warning,
                    "`os` imported but unused",
                    "ruff",
                )],
                cx,
            );
            assert_eq!(
                i.diagnostic_counts(),
                DiagnosticCounts {
                    errors: 1,
                    warnings: 1,
                    ..Default::default()
                }
            );
            i.replace_bytes(0..0, "# header\n", 9, cx);
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    window
        .update(visual, |i, _, cx| {
            let found: Vec<_> = i
                .external_diagnostics()
                .iter()
                .map(|d| (&i.content[d.range.clone()], d.source.to_string()))
                .collect();
            assert_eq!(
                found,
                [
                    ("os", "ruff".to_owned()),
                    ("undefined_name", "basedpyright".to_owned())
                ]
            );
            // The scrollbar marks them, and its hover names their source.
            let code = i.code.as_ref().unwrap();
            let marks: Vec<_> = code
                .markers
                .iter()
                .filter(|m| m.diagnostic)
                .map(|m| (code.row(m.pos), m.message.to_string()))
                .collect();
            assert_eq!(
                marks,
                [
                    (1, "ruff: `os` imported but unused".to_owned()),
                    (
                        2,
                        "basedpyright: \"undefined_name\" is not defined".to_owned()
                    ),
                ]
            );
            assert_eq!(i.problems().len(), 2);
            // A source replaces only its own list.
            i.set_external_diagnostics("ruff", Vec::new(), cx);
            assert_eq!(i.external_diagnostics().len(), 1);
            assert_eq!(i.diagnostic_counts().errors, 1);
            // Deleting the offending line takes its diagnostic with it.
            let at = i.content.find("value = ").unwrap();
            i.replace_bytes(at..i.content.len(), "", 0, cx);
            assert!(i.external_diagnostics().is_empty());
        })
        .unwrap();
}

#[derive(Default)]
struct FakeCompletion {
    requests: RefCell<Vec<(String, usize, CompletionTrigger)>>,
}
impl CompletionProvider for FakeCompletion {
    fn trigger_characters(&self, _: &DocumentInfo) -> SharedString {
        ".".into()
    }
    fn complete(&self, request: CompletionRequest, cx: &mut App) -> Task<Vec<CompletionItem>> {
        let text = request.at.text.clone();
        let at = request.at.offset;
        self.requests
            .borrow_mut()
            .push((text.to_string(), at, request.trigger));
        let start = links::word_at(&text, at).map_or(at, |word| word.start);
        let item = |label: &str, import: Option<&str>| CompletionItem {
            label: label.into(),
            kind: "class".into(),
            insert: label.into(),
            replace: start..at,
            additional_edits: import
                .map(|line| TextEdit {
                    range: 0..0,
                    text: line.into(),
                })
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let items = vec![
            item("partition", None),
            item("Path", Some("from pathlib import Path\n")),
            item("zip", None),
        ];
        // A server takes a moment; the caret can move on meanwhile.
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            items
        })
    }
    fn resolve(
        &self,
        _: &DocumentInfo,
        item: &CompletionItem,
        _: &mut App,
    ) -> Option<Task<Option<String>>> {
        Some(Task::ready(Some(format!(
            "Documentation of `{}`",
            item.label
        ))))
    }
}

#[gpui::test]
fn async_completion_drops_stale_answers_and_applies_the_import_with_the_item(
    cx: &mut TestAppContext,
) {
    let window = editor(cx);
    let provider = Rc::new(FakeCompletion::default());
    window
        .update(cx, |i, w, cx| {
            w.focus(&i.focus, cx);
            i.set_editor_path("/project/app.py".into());
            i.set_editor_providers(
                Providers {
                    completion: Some(provider.clone()),
                    ..Default::default()
                },
                cx,
            );
            i.set_text("p = Pa".into(), cx);
            i.reset_editor_history();
            i.schedule_completion(true, cx);
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    settle(visual, 10);
    assert_eq!(provider.requests.borrow().len(), 1);
    // The answer is still on its way when another character is typed.
    window
        .update(visual, |i, w, cx| i.replace_text_in_range(None, "t", w, cx))
        .unwrap();
    settle(visual, 200);
    window
        .update(visual, |i, _, cx| {
            assert!(i.completion_labels().is_empty(), "a stale answer was shown");
            i.schedule_completion(false, cx);
        })
        .unwrap();
    settle(visual, 100);
    settle(visual, 60);
    window
        .update(visual, |i, _, cx| {
            // "zip" does not match "Pat"; the prefix match leads.
            assert_eq!(i.completion_labels(), ["Path", "partition"]);
            assert_eq!(
                provider.requests.borrow()[1],
                ("p = Pat".to_owned(), 7, CompletionTrigger::Typed)
            );
            assert_eq!(
                i.completion_documentation_text().unwrap().as_ref(),
                "Documentation of `Path`"
            );
            assert!(i.accept_completion(cx));
            assert_eq!(i.content.as_ref(), "from pathlib import Path\np = Path");
            assert_eq!(i.cursor(), i.content.len());
        })
        .unwrap();
    // The import and the word are one step to undo.
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    window
        .update(visual, |i, w, cx| {
            assert_eq!(i.content.as_ref(), "p = Pat");
            i.set_text("os".into(), cx);
            // Accepting an item mutes the next automatic request; that was
            // spent on the edit a file pane would have reported.
            i.code.as_mut().unwrap().suppress_completion = false;
            i.replace_text_in_range(None, ".", w, cx);
            i.schedule_completion(false, cx);
        })
        .unwrap();
    settle(visual, 100);
    assert_eq!(
        provider.requests.borrow().last().unwrap().2,
        CompletionTrigger::Character('.')
    );
}

struct FakeInsight;
impl HoverProvider for FakeInsight {
    fn hover(&self, at: Position, _: &mut App) -> Task<Option<Hover>> {
        Task::ready(links::word_at(&at.text, at.offset).map(|word| Hover {
            markdown: format!(
                "```python\n{}: int\n```\nA **name**.",
                &at.text[word.clone()]
            ),
            range: Some(word),
        }))
    }
}
impl SignatureProvider for FakeInsight {
    fn signature(&self, at: Position, _: &mut App) -> Task<Option<Signature>> {
        let before = &at.text[..at.offset];
        let open = before
            .rfind('(')
            .filter(|open| !before[*open..].contains(')'));
        Task::ready(open.map(|open| Signature {
            label: "connect(host, port)".into(),
            parameters: vec![8..12, 14..18],
            active: Some(before[open..].matches(',').count()),
            documentation: Some("Opens a connection.".into()),
        }))
    }
}

#[gpui::test]
fn hover_shows_on_rest_and_on_key_and_signature_help_follows_the_caret(cx: &mut TestAppContext) {
    let window = editor(cx);
    // The second word sits clear of the first one's popover, which keeps
    // the mouse to itself while it is open.
    let text = "value = 1\n\n\n\n\n\n\n\n\n\nconnect\n";
    let word = text.find("connect").unwrap();
    window
        .update(cx, |i, w, cx| {
            w.focus(&i.focus, cx);
            i.set_editor_providers(
                Providers {
                    hover: Some(Rc::new(FakeInsight)),
                    signature: Some(Rc::new(FakeInsight)),
                    ..Default::default()
                },
                cx,
            );
            i.set_text(text.into(), cx);
            i.move_to(2, cx);
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    visual.simulate_keystrokes("f1");
    let over_connect = window
        .update(visual, |i, _, _| {
            let (range, markdown) = i.hover_text().expect("the key command shows the hover");
            assert_eq!(range, 0..5);
            assert!(markdown.contains("value: int"));
            glyph(i, text.find("connect").unwrap() + 2)
        })
        .unwrap();
    // Resting the mouse on another word replaces it, after the delay.
    visual.simulate_mouse_move(over_connect, None, Modifiers::default());
    visual.run_until_parked();
    window
        .update(visual, |i, _, _| assert!(i.hover_text().is_none()))
        .unwrap();
    settle(visual, 400);
    window
        .update(visual, |i, _, _| {
            assert_eq!(i.hover_text().unwrap().0, word..word + 7);
        })
        .unwrap();
    // Blank space has nothing to say, and typing closes whatever is open.
    visual.simulate_mouse_move(
        over_connect + point(px(200.), px(0.)),
        None,
        Modifiers::default(),
    );
    settle(visual, 400);
    window
        .update(visual, |i, w, cx| {
            assert!(i.hover_text().is_none());
            i.show_hover(&ShowHover, w, cx);
        })
        .unwrap();
    visual.run_until_parked();
    window
        .update(visual, |i, w, cx| {
            assert!(i.hover_text().is_some());
            i.move_to(text.find("connect").unwrap() + 7, cx);
            i.replace_text_in_range(None, "(", w, cx);
        })
        .unwrap();
    settle(visual, 60);
    let active = |visual: &mut VisualTestContext| {
        window
            .update(visual, |i, _, _| {
                i.signature_shown().map(|signature| signature.active)
            })
            .unwrap()
    };
    window
        .update(visual, |i, _, _| assert!(i.hover_text().is_none()))
        .unwrap();
    assert_eq!(active(visual), Some(Some(0)));
    for typed in ["a", ","] {
        window
            .update(visual, |i, w, cx| {
                i.replace_text_in_range(None, typed, w, cx)
            })
            .unwrap();
        settle(visual, 60);
    }
    assert_eq!(active(visual), Some(Some(1)), "the second parameter");
    window
        .update(visual, |i, w, cx| {
            i.replace_text_in_range(None, "b)", w, cx)
        })
        .unwrap();
    settle(visual, 60);
    assert_eq!(active(visual), None, "closed once outside the call");
}

struct FakeLinks {
    other: PathBuf,
    current: PathBuf,
}
impl FakeLinks {
    fn definitions(&self, at: &Position) -> Vec<LinkTarget> {
        let word = links::word_at(&at.text, at.offset).map(|word| &at.text[word]);
        let here = |needle: &str| LinkTarget {
            path: self.current.clone(),
            position: TargetPosition::Range({
                let start = at.text.find(needle).unwrap();
                start..start + needle.len()
            }),
        };
        match word {
            Some("helper") => vec![here("helper")],
            Some("many") => vec![
                here("many"),
                LinkTarget {
                    path: self.other.clone(),
                    position: TargetPosition::LineColumn { line: 2, column: 5 },
                },
            ],
            _ => Vec::new(),
        }
    }
}
impl LinkProvider for FakeLinks {
    fn navigable(&self, at: Position, _: &mut App) -> Task<bool> {
        Task::ready(!self.definitions(&at).is_empty())
    }
    fn targets(&self, at: Position, _: &mut App) -> Task<Vec<LinkTarget>> {
        Task::ready(self.definitions(&at))
    }
}
impl ReferencesProvider for FakeLinks {
    fn references(&self, at: Position, _: &mut App) -> Task<Vec<LinkTarget>> {
        Task::ready(
            at.text
                .match_indices("helper")
                .map(|(start, _)| LinkTarget {
                    path: self.current.clone(),
                    position: TargetPosition::Range(start..start + 6),
                })
                .collect(),
        )
    }
}

#[gpui::test]
fn links_underline_jump_list_several_targets_and_hand_usages_to_a_handler(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let current = dir.path().join("app.py");
    let other = dir.path().join("lib.py");
    std::fs::write(&other, "import os\ndef many():\n    pass\n").unwrap();
    let text =
        "def helper():\n    pass\n\ndef many():\n    pass\n\nprint(helper(), many(), helper)\n";
    let window = editor(cx);
    let provider = Rc::new(FakeLinks {
        other: other.clone(),
        current: current.clone(),
    });
    window
        .update(cx, |i, w, cx| {
            w.focus(&i.focus, cx);
            i.set_editor_document(Rc::new(DocumentInfo::new(&current, dir.path())), None);
            i.set_editor_providers(
                Providers {
                    links: Some(provider.clone()),
                    references: Some(provider.clone()),
                    ..Default::default()
                },
                cx,
            );
            i.set_text(text.into(), cx);
            i.move_to(text.len(), cx);
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let call = text.find("print(").unwrap() + 6;
    let (over_call, over_print) = window
        .update(visual, |i, _, _| {
            (
                glyph(i, call + 1),
                glyph(i, text.find("print").unwrap() + 1),
            )
        })
        .unwrap();
    let held = Modifiers::secondary_key();
    let link = |visual: &mut VisualTestContext| {
        window
            .update(visual, |i, _, _| {
                i.code
                    .as_ref()
                    .unwrap()
                    .decor
                    .as_ref()
                    .and_then(|d| d.link.clone())
            })
            .unwrap()
    };
    // Without the modifier nothing is a link; with it, only what resolves.
    visual.simulate_mouse_move(over_call, None, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(link(visual), None);
    visual.simulate_mouse_move(over_print, None, held);
    visual.run_until_parked();
    assert_eq!(link(visual), None);
    visual.simulate_mouse_move(over_call, None, held);
    visual.run_until_parked();
    assert_eq!(link(visual), Some(call..call + 6));
    visual.simulate_modifiers_change(Modifiers::default());
    visual.run_until_parked();
    assert_eq!(link(visual), None, "releasing the modifier drops the link");
    // One target: the click goes there and Back knows where it came from.
    visual.simulate_click(over_call, held);
    visual.run_until_parked();
    window
        .update(visual, |i, _, cx| {
            assert_eq!(i.cursor(), text.find("helper").unwrap());
            assert!(!i.selecting, "a link click starts no selection");
            let hub = crate::code_editor::hub(cx);
            assert!(hub.read(cx).history().can_back());
            let back = hub.update(cx, |hub, cx| hub.back(None, cx)).unwrap();
            assert_eq!((back.path, back.line), (current.clone(), 7));
            // Several targets: a list at the symbol, each with its line.
            i.move_to(text.rfind("many()").unwrap() + 2, cx);
        })
        .unwrap();
    visual.simulate_keystrokes("f12");
    window
        .update(visual, |i, _, _| {
            let listed: Vec<_> = i
                .picker_targets()
                .expect("two definitions open the list")
                .iter()
                .map(|t| {
                    (
                        t.location
                            .path
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                        t.location.line,
                        t.text.clone(),
                    )
                })
                .collect();
            assert_eq!(
                listed,
                [
                    ("app.py".to_owned(), 4, "def many():".to_owned()),
                    ("lib.py".to_owned(), 2, "def many():".to_owned()),
                ]
            );
        })
        .unwrap();
    // Picking the first row jumps within the file.
    visual.simulate_keystrokes("enter");
    window
        .update(visual, |i, _, cx| {
            assert!(i.picker_targets().is_none());
            assert_eq!(i.cursor(), text.find("many").unwrap());
            i.move_to(call + 1, cx);
        })
        .unwrap();
    // Usages go to the handler when there is one, to the list otherwise.
    let handed: Rc<RefCell<Vec<References>>> = Rc::default();
    visual.update(|_, cx| {
        let handed = handed.clone();
        crate::code_editor::hub(cx).update(cx, |hub, _| {
            hub.set_references_handler(Some(Rc::new(move |references, _, _| {
                handed.borrow_mut().push(references.clone());
                true
            })))
        });
    });
    visual.simulate_keystrokes("shift-f12");
    window
        .update(visual, |i, _, cx| {
            assert!(i.picker_targets().is_none());
            let handed = handed.borrow();
            assert_eq!(handed[0].symbol, "helper");
            assert_eq!(handed[0].origin.line, 7);
            let lines: Vec<_> = handed[0].targets.iter().map(|t| t.location.line).collect();
            assert_eq!(lines, [1, 7, 7], "two usages on one line are two places");
            crate::code_editor::hub(cx).update(cx, |hub, _| hub.set_references_handler(None));
        })
        .unwrap();
    visual.simulate_keystrokes("shift-f12");
    window
        .update(visual, |i, _, _| {
            assert_eq!(i.picker_targets().map(<[_]>::len), Some(3));
        })
        .unwrap();
}

#[derive(Default)]
struct FakeGutter {
    clicks: RefCell<Vec<GutterClick>>,
    moved: RefCell<Vec<Vec<usize>>>,
}
impl GutterProvider for FakeGutter {
    fn click(&self, _: &DocumentInfo, click: &GutterClick, _: &mut Window, _: &mut App) {
        self.clicks.borrow_mut().push(click.clone());
    }
    fn moved(&self, _: &DocumentInfo, marks: &[GutterMark], _: &mut App) {
        self.moved
            .borrow_mut()
            .push(marks.iter().map(|mark| mark.row).collect());
    }
}

#[gpui::test]
fn gutter_presses_reach_their_provider_with_the_right_row_past_folds_and_wraps(
    cx: &mut TestAppContext,
) {
    let window = editor(cx);
    let long = "wrapped ".repeat(40);
    let source = format!(
        "function hello() {{\n  const a = 1;\n  return a;\n}}\n// {long}\nlet last = 1;\nlet more = 2;\n"
    );
    let provider = Rc::new(FakeGutter::default());
    let plain = window
        .update(cx, |i, w, cx| {
            w.focus(&i.focus, cx);
            i.set_text(source.clone(), cx);
            i.set_editor_analysis(
                crate::editor_analysis::analyze("a.js", &source, None).unwrap(),
                cx,
            );
            i.toggle_fold_row(0, cx);
            i.move_to(source.len(), cx);
            // Nothing installed: the gutter is as narrow as it always was.
            i.code.as_ref().unwrap().gutter()
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    window
        .update(visual, |i, _, cx| {
            assert!(i.code.as_ref().unwrap().decor.is_none());
            assert!(
                i.gutter_hit(i.bounds.unwrap().origin - point(px(40.), px(-20.)))
                    .is_none()
            );
            i.set_editor_providers(
                Providers {
                    breakpoints: Some(provider.clone()),
                    run_markers: Some(provider.clone()),
                    ..Default::default()
                },
                cx,
            );
            i.set_gutter_marks(
                GutterSlot::Run,
                vec![GutterMark {
                    row: 6,
                    kind: GutterMarkKind::Run,
                    tooltip: "Run".into(),
                }],
                cx,
            );
            i.set_gutter_marks(
                GutterSlot::Breakpoint,
                vec![GutterMark {
                    row: 5,
                    kind: GutterMarkKind::Breakpoint(Breakpoint::Conditional),
                    tooltip: "a > 1".into(),
                }],
                cx,
            );
        })
        .unwrap();
    visual.run_until_parked();
    let (wrapped, breakpoint, empty, run) = window
        .update(visual, |i, _, _| {
            let code = i.code.as_ref().unwrap();
            assert_eq!(code.gutter(), plain + 2. * SLOT_WIDTH);
            assert!(code.hidden(1) && code.hidden(3), "the function is folded");
            assert!(
                code.tops[5] - code.tops[4] > i.line_height * 2.,
                "the comment wraps"
            );
            (
                // The second visual line of the wrapped row.
                i.gutter_slot_center(GutterSlot::Breakpoint, 4).unwrap()
                    + point(px(0.), px(i.line_height)),
                i.gutter_slot_center(GutterSlot::Breakpoint, 5).unwrap(),
                i.gutter_slot_center(GutterSlot::Breakpoint, 6).unwrap(),
                i.gutter_slot_center(GutterSlot::Run, 6).unwrap(),
            )
        })
        .unwrap();
    let caret = window.update(visual, |i, _, _| i.cursor()).unwrap();
    visual.simulate_click(wrapped, Modifiers::default());
    visual.simulate_click(breakpoint, Modifiers::default());
    visual.simulate_mouse_move(empty, None, Modifiers::default());
    visual.run_until_parked();
    window
        .update(visual, |i, _, _| {
            let code = i.code.as_ref().unwrap();
            assert_eq!(code.decor.as_ref().unwrap().hover, Some(6), "the faint dot");
        })
        .unwrap();
    visual.simulate_mouse_down(run, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(run, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_move(run, None, Modifiers::default());
    visual.run_until_parked();
    window
        .update(visual, |i, _, cx| {
            let clicks = provider.clicks.borrow();
            let seen: Vec<_> = clicks
                .iter()
                .map(|c| (c.slot, c.row, c.mark, c.button))
                .collect();
            use GutterSlot::{Breakpoint as Column, Run};
            assert_eq!(
                seen,
                [
                    (Column, 4, None, GutterButton::Left),
                    (
                        Column,
                        5,
                        Some(GutterMarkKind::Breakpoint(Breakpoint::Conditional)),
                        GutterButton::Left
                    ),
                    (Run, 6, Some(GutterMarkKind::Run), GutterButton::Right),
                ]
            );
            assert_eq!(clicks[2].position, run);
            assert_eq!(i.cursor(), caret, "the gutter does not move the caret");
            assert!(i.context_menu.is_none(), "nor open the editing menu");
            let tip = &i.code.as_ref().unwrap().ext.as_ref().unwrap().gutter_tip;
            assert_eq!(tip.as_ref().map(|(text, _)| text.as_ref()), Some("Run"));
            // A line typed above carries the marks down with their lines.
            i.replace_bytes(0..0, "// top\n", 7, cx);
            assert_eq!(i.gutter_marks(GutterSlot::Breakpoint)[0].row, 6);
            assert_eq!(i.gutter_marks(GutterSlot::Run)[0].row, 7);
        })
        .unwrap();
    visual.run_until_parked();
    assert_eq!(*provider.moved.borrow(), [vec![6], vec![7]]);
}

#[gpui::test]
fn line_decorations_and_notes_draw_follow_edits_and_reveal_without_moving_the_caret(
    cx: &mut TestAppContext,
) {
    let window = editor(cx);
    let text: String = (0..200).map(|n| format!("line_{n} = {n}\n")).collect();
    window
        .update(cx, |i, w, cx| {
            w.focus(&i.focus, cx);
            i.set_text(text.clone(), cx);
            i.move_to(0, cx);
            i.set_line_decorations(
                vec![
                    LineDecoration {
                        row: 3,
                        style: LineStyle::Execution,
                        arrow: true,
                    },
                    LineDecoration {
                        row: 150,
                        style: LineStyle::Frame,
                        arrow: false,
                    },
                ],
                cx,
            );
            i.set_inline_notes(
                vec![InlineNote {
                    row: 3,
                    text: "line_3: 3".into(),
                    tone: NoteTone::Dim,
                }],
                cx,
            );
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    window
        .update(visual, |i, _, cx| {
            let decor = i.code.as_ref().unwrap().decor.clone().unwrap();
            // The arrow needs the breakpoint column even with no provider.
            assert_eq!(decor.slot_left(GutterSlot::Breakpoint), Some(4.));
            assert_eq!(decor.slot_left(GutterSlot::Run), None);
            assert_eq!(decor.notes_on(3).len(), 1);
            assert_eq!(i.scroll_y, px(0.));
            i.reveal_row(150, cx);
        })
        .unwrap();
    visual.run_until_parked();
    window
        .update(visual, |i, _, cx| {
            let code = i.code.as_ref().unwrap();
            let top = code.top(150, i.line_height);
            assert!(i.scroll_y.as_f32() < top && top < i.scroll_y.as_f32() + 300.);
            assert_eq!(i.cursor(), 0, "revealing leaves the caret");
            let at = text.find("line_2 ").unwrap();
            i.replace_bytes(at..at, "inserted = 0\nagain = 0\n", 0, cx);
            let decor = i.code.as_ref().unwrap().decor.clone().unwrap();
            assert_eq!(
                decor.lines.iter().map(|l| l.row).collect::<Vec<_>>(),
                [5, 152]
            );
            assert_eq!(decor.notes[0].row, 5);
            i.set_line_decorations(Vec::new(), cx);
            i.set_inline_notes(Vec::new(), cx);
            assert_eq!(i.code.as_ref().unwrap().gutter(), {
                let mut plain = Code::new();
                plain.offsets = i.code.as_ref().unwrap().offsets.clone();
                plain.gutter()
            });
        })
        .unwrap();
    visual.run_until_parked();
}

/// An editor for `path` holding `text`, focused and drawn once.
fn focused_editor(cx: &mut TestAppContext, path: &str, text: &str) -> WindowHandle<Input> {
    let window = editor(cx);
    window
        .update(cx, |i, w, cx| {
            w.focus(&i.focus, cx);
            i.set_editor_path(path.into());
            i.set_text(text.into(), cx);
            i.select_range(0..0, cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
}
fn modifier() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    }
}
/// What every caret selects, in document order, the primary one marked.
fn carets(window: WindowHandle<Input>, cx: &mut TestAppContext) -> Vec<(Range<usize>, bool)> {
    window
        .update(cx, |i, _, _| {
            let primary = i.selection.clone();
            let mut all: Vec<_> = i
                .all_carets()
                .into_iter()
                .map(|c| (c.range.clone(), c.range == primary))
                .collect();
            all.sort_by_key(|(r, _)| r.start);
            all
        })
        .unwrap()
}
fn text_of(window: WindowHandle<Input>, cx: &mut TestAppContext) -> String {
    window.update(cx, |i, _, _| i.content.to_string()).unwrap()
}

#[gpui::test]
fn extend_selection_walks_out_through_the_syntax_and_shrink_walks_back(cx: &mut TestAppContext) {
    let text = "function f() {\n  call(a, \"some text\");\n}\n";
    let window = focused_editor(cx, "a.js", text);
    let at = text.find("some").unwrap();
    window
        .update(cx, |i, _, cx| i.select_range(at + 1..at + 1, cx))
        .unwrap();
    let selected = |window: WindowHandle<Input>, cx: &mut TestAppContext| {
        window
            .update(cx, |i, _, _| i.content[i.selection.clone()].to_owned())
            .unwrap()
    };
    let steps = [
        "some",
        "some text",
        "\"some text\"",
        "a, \"some text\"",
        "(a, \"some text\")",
        "call(a, \"some text\")",
        "call(a, \"some text\");",
    ];
    for step in steps {
        cx.simulate_keystrokes(window.into(), "alt-up");
        assert_eq!(selected(window, cx), step);
    }
    for _ in 0..10 {
        cx.simulate_keystrokes(window.into(), "alt-up");
    }
    assert_eq!(selected(window, cx), text, "it ends at the whole file");
    for _ in 0..20 {
        cx.simulate_keystrokes(window.into(), "alt-down");
    }
    assert_eq!(
        carets(window, cx),
        [(at + 1..at + 1, true)],
        "back at the caret"
    );
    cx.simulate_keystrokes(window.into(), "alt-up alt-up");
    assert_eq!(selected(window, cx), "some text");
    // Any other selection change forgets the way back.
    cx.simulate_keystrokes(window.into(), "shift-right alt-down");
    assert_eq!(selected(window, cx), "some text\"");
}

#[gpui::test]
fn extend_selection_without_a_grammar_uses_words_quotes_brackets_and_lines(
    cx: &mut TestAppContext,
) {
    let text = "say(\"hi there\", [1, 2])\nnext";
    let window = focused_editor(cx, "notes.unknown", text);
    let at = text.find("there").unwrap();
    window
        .update(cx, |i, _, cx| i.select_range(at..at, cx))
        .unwrap();
    let mut seen = Vec::new();
    for _ in 0..7 {
        cx.simulate_keystrokes(window.into(), "alt-up");
        seen.push(
            window
                .update(cx, |i, _, _| i.content[i.selection.clone()].to_owned())
                .unwrap(),
        );
    }
    assert_eq!(
        seen,
        [
            "there",
            "hi there",
            "\"hi there\"",
            "\"hi there\", [1, 2]",
            "(\"hi there\", [1, 2])",
            "say(\"hi there\", [1, 2])",
            text,
        ]
    );
}

#[gpui::test]
fn duplicate_line_copies_the_line_the_selection_or_the_selected_lines(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "a\nbc\nd");
    window
        .update(cx, |i, w, cx| {
            i.select_range(3..3, cx);
            i.duplicate_line(&DuplicateLine, w, cx);
            assert_eq!(i.content.as_ref(), "a\nbc\nbc\nd");
            assert_eq!(i.selection, 6..6, "the caret moves to the copy");
            i.undo(&Undo, w, cx);
            assert_eq!(i.content.as_ref(), "a\nbc\nd");
            assert_eq!(i.selection, 3..3);
            // A selection inside a line is repeated after itself.
            i.select_range(2..4, cx);
            i.duplicate_line(&DuplicateLine, w, cx);
            assert_eq!(i.content.as_ref(), "a\nbcbc\nd");
            assert_eq!(i.selection, 4..6);
            i.undo(&Undo, w, cx);
            // A selection over two lines duplicates both, whole.
            i.select_range(0..3, cx);
            i.duplicate_line(&DuplicateLine, w, cx);
            assert_eq!(i.content.as_ref(), "a\nbc\na\nbc\nd");
            assert_eq!(i.selection, 5..8);
            i.undo(&Undo, w, cx);
            assert_eq!(i.content.as_ref(), "a\nbc\nd");
            assert!(!i.history_available(false));
        })
        .unwrap();
}

#[gpui::test]
fn move_line_swaps_with_neighbours_keeps_the_selection_and_stops_at_the_edges(
    cx: &mut TestAppContext,
) {
    let window = focused_editor(cx, "a.txt", "one\ntwo\nthree");
    let m = modifier();
    window
        .update(cx, |i, _, cx| i.select_range(5..5, cx))
        .unwrap();
    cx.simulate_keystrokes(window.into(), "alt-shift-up");
    assert_eq!(text_of(window, cx), "two\none\nthree");
    assert_eq!(carets(window, cx), [(1..1, true)]);
    cx.simulate_keystrokes(window.into(), "alt-shift-up");
    assert_eq!(text_of(window, cx), "two\none\nthree", "the top stops it");
    cx.simulate_keystrokes(window.into(), "alt-shift-down alt-shift-down");
    assert_eq!(text_of(window, cx), "one\nthree\ntwo");
    assert_eq!(carets(window, cx), [(11..11, true)]);
    cx.simulate_keystrokes(window.into(), "alt-shift-down");
    assert_eq!(
        text_of(window, cx),
        "one\nthree\ntwo",
        "the bottom stops it"
    );
    // Each move is its own undo step.
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "one\ntwo\nthree");
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "two\none\nthree");
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "one\ntwo\nthree");
    // Several selected lines move together and stay selected.
    window
        .update(cx, |i, _, cx| i.select_range(1..6, cx))
        .unwrap();
    cx.simulate_keystrokes(window.into(), "alt-shift-down");
    assert_eq!(text_of(window, cx), "three\none\ntwo");
    assert_eq!(carets(window, cx), [(7..12, true)]);
}

#[gpui::test]
fn moving_a_line_over_a_folded_region_jumps_it_whole_and_keeps_it_folded(cx: &mut TestAppContext) {
    let text = "let a = 1;\nfunction f() {\n  return 1;\n}\nlet b = 2;";
    let window = focused_editor(cx, "a.js", text);
    window
        .update(cx, |i, _, cx| {
            i.set_editor_analysis(
                crate::editor_analysis::analyze("a.js", text, None).unwrap(),
                cx,
            );
            i.toggle_fold_row(1, cx);
            i.select_range(0..0, cx);
        })
        .unwrap();
    cx.run_until_parked();
    cx.simulate_keystrokes(window.into(), "alt-shift-down");
    window
        .update(cx, |i, w, cx| {
            assert_eq!(
                i.content.as_ref(),
                "function f() {\n  return 1;\n}\nlet a = 1;\nlet b = 2;"
            );
            let code = i.code.as_ref().unwrap();
            assert_eq!(code.folded.len(), 1);
            let fold = &code.folded[0];
            assert_eq!(&i.content[fold.from..fold.to], "\n  return 1;\n");
            assert_eq!(code.row(i.cursor()), 3);
            // Moving the folded function itself takes all of it.
            i.select_range(0..0, cx);
            i.move_line_down(&MoveLineDown, w, cx);
            assert_eq!(
                i.content.as_ref(),
                "let a = 1;\nfunction f() {\n  return 1;\n}\nlet b = 2;"
            );
            let code = i.code.as_ref().unwrap();
            assert_eq!(code.folded.len(), 1);
            assert_eq!(code.row(code.folded[0].from), 1);
        })
        .unwrap();
}

#[gpui::test]
fn carets_added_above_and_below_keep_their_column_and_skip_short_lines(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "abcd\na\nabcd\nabcd");
    window
        .update(cx, |i, w, cx| {
            i.select_range(3..3, cx);
            i.add_caret_below(&AddCaretBelow, w, cx);
        })
        .unwrap();
    // Row 1 is too short for column 3.
    assert_eq!(carets(window, cx), [(3..3, false), (10..10, true)]);
    window
        .update(cx, |i, w, cx| {
            i.add_caret_below(&AddCaretBelow, w, cx);
            i.add_caret_below(&AddCaretBelow, w, cx);
            i.add_caret_above(&AddCaretAbove, w, cx);
        })
        .unwrap();
    // Nothing is below the last line or above the first.
    assert_eq!(
        carets(window, cx),
        [(3..3, false), (10..10, false), (15..15, true)]
    );
    cx.simulate_keystrokes(window.into(), "escape");
    assert_eq!(carets(window, cx).len(), 1, "Escape leaves one caret");
}

#[gpui::test]
fn several_carets_type_delete_move_and_undo_as_one(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "abc\nabc\nabc");
    let m = modifier();
    window
        .update(cx, |i, w, cx| {
            i.select_range(1..1, cx);
            i.add_caret_below(&AddCaretBelow, w, cx);
            i.add_caret_below(&AddCaretBelow, w, cx);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "x y");
    assert_eq!(text_of(window, cx), "axybc\naxybc\naxybc");
    cx.simulate_keystrokes(window.into(), "backspace");
    assert_eq!(text_of(window, cx), "axbc\naxbc\naxbc");
    cx.simulate_keystrokes(window.into(), "delete");
    assert_eq!(text_of(window, cx), "axc\naxc\naxc");
    cx.simulate_keystrokes(window.into(), "end z");
    assert_eq!(text_of(window, cx), "axcz\naxcz\naxcz");
    cx.simulate_keystrokes(window.into(), "home left");
    // The first caret cannot go left of the buffer start; the others wrap.
    assert_eq!(
        carets(window, cx)
            .iter()
            .map(|(r, _)| r.start)
            .collect::<Vec<_>>(),
        [0, 4, 9]
    );
    cx.simulate_keystrokes(window.into(), "shift-enter");
    assert_eq!(text_of(window, cx), "\naxcz\n\naxcz\n\naxcz");
    // Each edit with every caret is one step, and undo brings the carets back.
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "axcz\naxcz\naxcz");
    assert_eq!(carets(window, cx).len(), 3);
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "axc\naxc\naxc");
    // Backspace and Delete in a row are one deleting step, as with one caret.
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "axybc\naxybc\naxybc");
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "abc\nabc\nabc");
    assert_eq!(carets(window, cx).len(), 3);
    // A click leaves one caret.
    window
        .update(cx, |i, _, cx| i.select_range(0..0, cx))
        .unwrap();
    assert_eq!(carets(window, cx).len(), 1);
}

#[gpui::test]
fn several_carets_copy_cut_and_paste_a_line_each(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "one\ntwo\nsix");
    let m = modifier();
    window
        .update(cx, |i, w, cx| {
            i.select_range(0..0, cx);
            i.add_caret_below(&AddCaretBelow, w, cx);
            i.add_caret_below(&AddCaretBelow, w, cx);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), "shift-right shift-right");
    cx.simulate_keystrokes(window.into(), &format!("{m}-c"));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("on\ntw\nsi")
    );
    cx.simulate_keystrokes(window.into(), &format!("{m}-x"));
    assert_eq!(text_of(window, cx), "e\no\nx");
    // As many lines as carets: one line each, in order.
    cx.simulate_keystrokes(window.into(), &format!("{m}-v"));
    assert_eq!(text_of(window, cx), "one\ntwo\nsix");
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("-".into()));
    cx.simulate_keystrokes(window.into(), &format!("{m}-v"));
    assert_eq!(text_of(window, cx), "on-e\ntw-o\nsi-x");
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "one\ntwo\nsix");
}

#[gpui::test]
fn column_selection_mode_selects_a_rectangle_one_caret_per_line(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "abcdef\nab\nabcdef");
    let m = modifier();
    window
        .update(cx, |i, _, cx| i.select_range(1..1, cx))
        .unwrap();
    cx.simulate_keystrokes(window.into(), &format!("{m}-shift-8"));
    assert!(window.update(cx, |i, _, _| i.column_selection()).unwrap());
    cx.simulate_keystrokes(
        window.into(),
        "shift-down shift-down shift-right shift-right shift-right",
    );
    // The short middle line is selected as far as it goes.
    assert_eq!(
        carets(window, cx),
        [(1..4, false), (8..9, false), (11..14, true)]
    );
    cx.simulate_keystrokes(window.into(), &format!("{m}-c"));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("bcd\nb\nbcd")
    );
    cx.simulate_keystrokes(window.into(), "z");
    assert_eq!(text_of(window, cx), "azef\naz\nazef");
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "abcdef\nab\nabcdef");
    // Out of column mode Shift+Down selects text again.
    cx.simulate_keystrokes(window.into(), &format!("{m}-shift-8"));
    window
        .update(cx, |i, _, cx| i.select_range(1..1, cx))
        .unwrap();
    cx.simulate_keystrokes(window.into(), "shift-down");
    assert_eq!(carets(window, cx), [(1..8, true)]);
}

#[gpui::test]
fn alt_shift_click_adds_and_removes_carets(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "abc\nabc");
    window
        .update(cx, |i, _, cx| {
            i.select_range(1..1, cx);
            i.toggle_caret_at(6, cx);
            assert_eq!(i.all_carets().len(), 2);
            assert_eq!(i.selection, 6..6);
            i.toggle_caret_at(1, cx);
            assert_eq!(i.all_carets().len(), 1);
            assert_eq!(i.selection, 6..6);
        })
        .unwrap();
}

#[gpui::test]
fn dragging_in_column_mode_selects_a_rectangle(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "abcdef\nabcdef\nabcdef");
    window
        .update(cx, |i, w, cx| {
            i.toggle_column_selection(&ToggleColumnSelection, w, cx)
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let (from, to) = window
        .update(visual, |i, _, _| {
            let at = |offset| {
                i.bounds.unwrap().origin + i.code_position(offset) - point(i.scroll_x, i.scroll_y)
                    + point(px(1.), px(10.))
            };
            (at(1), at(18))
        })
        .unwrap();
    visual.simulate_mouse_down(from, MouseButton::Left, Default::default());
    visual.simulate_mouse_move(to, MouseButton::Left, Default::default());
    visual.simulate_mouse_up(to, MouseButton::Left, Default::default());
    visual.run_until_parked();
    window
        .update(visual, |i, _, _| {
            let mut all: Vec<_> = i.all_carets().into_iter().map(|c| c.range).collect();
            all.sort_by_key(|r| r.start);
            assert_eq!(all, [1..4, 8..11, 15..18]);
        })
        .unwrap();
}

/// Puts the carets at `ranges`, the first one primary.
fn set_carets(window: WindowHandle<Input>, cx: &mut TestAppContext, ranges: &[Range<usize>]) {
    window
        .update(cx, |i, _, cx| {
            i.select_range(0..0, cx);
            i.set_carets(
                ranges
                    .iter()
                    .map(|range| Caret {
                        range: range.clone(),
                        reversed: false,
                    })
                    .collect(),
            );
        })
        .unwrap();
}
fn heads(window: WindowHandle<Input>, cx: &mut TestAppContext) -> Vec<usize> {
    carets(window, cx).iter().map(|(r, _)| r.end).collect()
}

#[gpui::test]
fn several_carets_toggle_comments_on_each_line_once(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.py", "ab\ncd\nef");
    let m = modifier();
    // Two carets on the first line comment it once.
    set_carets(window, cx, &[1..1, 2..2, 7..7]);
    cx.simulate_keystrokes(window.into(), &format!("{m}-/"));
    assert_eq!(text_of(window, cx), "# ab\ncd\n# ef");
    assert_eq!(heads(window, cx), [3, 4, 11], "each caret keeps its place");
    cx.simulate_keystrokes(window.into(), &format!("{m}-/"));
    assert_eq!(text_of(window, cx), "ab\ncd\nef");
    // A selection over two lines and a caret below share one block.
    set_carets(window, cx, &[0..4, 7..7]);
    cx.simulate_keystrokes(window.into(), &format!("{m}-/"));
    assert_eq!(text_of(window, cx), "# ab\n# cd\n# ef");
    // The selection still covers its lines whole, as with one caret.
    assert_eq!(carets(window, cx), [(0..8, true), (13..13, false)]);
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "ab\ncd\nef", "one undo step");
    assert_eq!(carets(window, cx).len(), 2);
}

#[gpui::test]
fn several_carets_indent_and_outdent_every_line_they_select_once(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "a\nb\nc\nd");
    set_carets(window, cx, &[0..3, 2..2, 6..7]);
    cx.simulate_keystrokes(window.into(), "tab");
    assert_eq!(text_of(window, cx), "    a\n    b\nc\n    d");
    cx.simulate_keystrokes(window.into(), "shift-tab");
    assert_eq!(text_of(window, cx), "a\nb\nc\nd");
}

#[gpui::test]
fn several_carets_duplicate_their_lines_or_selections(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "a\nbc\nd");
    let m = modifier();
    // Two carets on one line copy it once; a selection in a line repeats.
    set_carets(window, cx, &[0..0, 1..1, 2..3, 6..6]);
    window
        .update(cx, |i, w, cx| i.duplicate_line(&DuplicateLine, w, cx))
        .unwrap();
    assert_eq!(text_of(window, cx), "a\na\nbbc\nd\nd");
    assert_eq!(
        carets(window, cx),
        [(2..2, true), (3..3, false), (5..6, false), (11..11, false)]
    );
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "a\nbc\nd");
}

#[gpui::test]
fn several_carets_move_their_lines_together_and_stop_at_the_edges(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "a\nb\nc\nd\ne");
    let m = modifier();
    set_carets(window, cx, &[2..2, 6..6]);
    cx.simulate_keystrokes(window.into(), "alt-shift-up");
    assert_eq!(text_of(window, cx), "b\na\nd\nc\ne");
    assert_eq!(heads(window, cx), [0, 4]);
    cx.simulate_keystrokes(window.into(), "alt-shift-up");
    assert_eq!(text_of(window, cx), "b\na\nd\nc\ne", "the top stops all");
    cx.simulate_keystrokes(window.into(), "alt-shift-down");
    assert_eq!(text_of(window, cx), "a\nb\nc\nd\ne");
    assert_eq!(heads(window, cx), [2, 6]);
    // Carets on touching lines move as one run.
    set_carets(window, cx, &[2..2, 4..4]);
    cx.simulate_keystrokes(window.into(), "alt-shift-up");
    assert_eq!(text_of(window, cx), "b\nc\na\nd\ne");
    assert_eq!(heads(window, cx), [0, 2]);
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), "a\nb\nc\nd\ne");
}

#[gpui::test]
fn a_completion_goes_in_at_every_caret_replacing_matching_prefixes(cx: &mut TestAppContext) {
    let text = "p = Pa\nq = Pa\nr = x";
    let window = focused_editor(cx, "a.py", text);
    let m = modifier();
    set_carets(window, cx, &[6..6, 13..13, 19..19]);
    window
        .update(cx, |i, _, cx| {
            // The popup follows the primary caret while there are others.
            i.schedule_completion(true, cx);
            assert!(i.code.as_ref().unwrap().completion_source.is_some());
            i.apply_completion(
                CompletionItem {
                    label: "Path".into(),
                    insert: "Path".into(),
                    replace: 4..6,
                    additional_edits: vec![TextEdit {
                        range: 0..0,
                        text: "from pathlib import Path\n".into(),
                    }],
                    ..Default::default()
                },
                cx,
            );
            assert_eq!(
                i.content.as_ref(),
                "from pathlib import Path\np = Path\nq = Path\nr = xPath"
            );
            assert_eq!(
                &i.content[..i.cursor()],
                "from pathlib import Path\np = Path"
            );
        })
        .unwrap();
    assert_eq!(heads(window, cx), [33, 42, 52]);
    cx.simulate_keystrokes(window.into(), &format!("{m}-z"));
    assert_eq!(text_of(window, cx), text, "one undo step");
}

#[gpui::test]
fn select_next_occurrence_adds_whole_words_from_a_caret_and_select_all_takes_every_match(
    cx: &mut TestAppContext,
) {
    let [_, _, _, next, all] = caret_keys();
    let window = focused_editor(cx, "a.txt", "foo foobar foo\nfoo");
    window
        .update(cx, |i, _, cx| i.select_range(1..1, cx))
        .unwrap();
    cx.simulate_keystrokes(window.into(), next);
    assert_eq!(carets(window, cx), [(0..3, true)], "first the word");
    // From a bare caret only whole words match: "foobar" is skipped.
    cx.simulate_keystrokes(window.into(), next);
    assert_eq!(carets(window, cx), [(0..3, false), (11..14, true)]);
    cx.simulate_keystrokes(window.into(), &format!("{next} {next}"));
    assert_eq!(
        carets(window, cx),
        [(0..3, false), (11..14, false), (15..18, true)],
        "nothing is left after wrapping around"
    );
    cx.simulate_keystrokes(window.into(), "x");
    assert_eq!(text_of(window, cx), "x foobar x\nx");
    // A selection made by hand matches inside words too.
    window
        .update(cx, |i, _, cx| {
            i.set_text("ab ab abc".into(), cx);
            i.select_range(3..5, cx);
        })
        .unwrap();
    cx.simulate_keystrokes(window.into(), all);
    assert_eq!(
        carets(window, cx),
        [(0..2, false), (3..5, true), (6..8, false)]
    );
}

#[gpui::test]
fn caret_keys_reach_the_editor_and_leave_the_workspace_keys_alone(cx: &mut TestAppContext) {
    let window = focused_editor(cx, "a.txt", "ab\nab\nab");
    cx.update(|cx| {
        crate::shell::bind_keys(cx);
        crate::file_editor::bind(cx);
    });
    let names = [
        "composer::DuplicateLine",
        "composer::AddCaretAbove",
        "composer::AddCaretBelow",
        "composer::SelectNextOccurrence",
        "composer::SelectAllOccurrences",
    ];
    let bound = |cx: &mut TestAppContext, key: &str| -> Vec<&'static str> {
        let key = gpui::Keystroke::parse(key).unwrap().unparse();
        cx.update(|cx| {
            cx.key_bindings()
                .borrow()
                .bindings()
                .filter(|b| b.keystrokes().len() == 1 && b.keystrokes()[0].unparse() == key)
                .map(|b| b.action().name())
                .collect()
        })
    };
    // Nothing else anywhere in the app takes these keys.
    for (key, name) in caret_keys().into_iter().zip(names) {
        assert_eq!(bound(cx, key), [name], "{key}");
    }
    // The workspace's split keys are not bound in the editor. (Browser
    // mode binds ⌘D to its favourites, only while it is on screen.)
    let m = modifier();
    let outside_browse = |names: Vec<&'static str>| -> Vec<&'static str> {
        names.into_iter().filter(|n| !n.starts_with("workspace::Browse")).collect()
    };
    assert_eq!(outside_browse(bound(cx, &format!("{m}-d"))), ["workspace::SplitRight"]);
    assert_eq!(bound(cx, &format!("{m}-shift-d")), ["workspace::SplitDown"]);
    let [duplicate, above, below, _, _] = caret_keys();
    window
        .update(cx, |i, _, cx| i.select_range(4..4, cx))
        .unwrap();
    cx.simulate_keystrokes(window.into(), &format!("{above} {below}"));
    assert_eq!(heads(window, cx), [1, 4, 7]);
    cx.simulate_keystrokes(window.into(), duplicate);
    assert_eq!(text_of(window, cx), "ab\nab\nab\nab\nab\nab");
}
