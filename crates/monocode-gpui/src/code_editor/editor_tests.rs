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
