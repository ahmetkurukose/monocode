//! The code editor's right-click menu, grouped as an IDE's is: chat and AI
//! first, then clipboard, navigation, folding, editing, the data sources,
//! search, Git and the places the file opens in. It replaces both the shared
//! text menu and the floating bar a selection used to show.
use super::{
    CompareWithClipboard, FileEditor, FileEvent, Find, Format, NavigateBack, NavigateForward,
    Replace, ToggleWordWrap,
};
use crate::{
    input,
    popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, MenuStyle, menu_popover},
};
use gpui::{
    Action, App, AppContext, ClipboardItem, Context, Entity, Focusable, IntoElement, Pixels, Point,
    Subscription, Window,
};

pub(super) struct CodeMenu {
    menu: Entity<Menu>,
    position: Point<Pixels>,
    _events: Subscription,
}

const MENU_WIDTH: f32 = 280.;
const SUBMENU_WIDTH: f32 = 248.;

const REVEAL_LABEL: &str = if cfg!(target_os = "macos") {
    "Finder"
} else if cfg!(target_os = "windows") {
    "File Explorer"
} else {
    "File Manager"
};

/// A shortcut as this platform writes it.
fn keys(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
}

/// The Refactor submenu: code action kinds asked for by name.
const REFACTOR_KINDS: [(&str, &str); 5] = [
    ("refactor.extract", "Extract…"),
    ("refactor.inline", "Inline…"),
    ("refactor.rewrite", "Rewrite…"),
    ("source.organizeImports", "Organize Imports"),
    ("source.fixAll", "Fix All"),
];

/// Prompts the AI actions put in the chat draft, ahead of the reference.
const AI_PROMPTS: [(&str, &str, &str); 5] = [
    ("ai-explain", "Explain Code", "Explain what this code does:"),
    (
        "ai-improve",
        "Suggest Improvements",
        "Suggest improvements to this code:",
    ),
    (
        "ai-problems",
        "Find Problems",
        "Find bugs and problems in this code:",
    ),
    ("ai-tests", "Write Tests", "Write tests for this code:"),
    (
        "ai-docs",
        "Write Documentation",
        "Write documentation comments for this code:",
    ),
];

impl FileEditor {
    fn code_menu_items(&self, sources: &[crate::database::Target], cx: &App) -> Vec<MenuItem> {
        let input = self.input.read(cx);
        let caps = input.editing_caps();
        let selected = !input.selection_range().is_empty();
        let clipboard = cx.read_from_clipboard().is_some();
        let writable = !caps.read_only;
        let action = |id: &'static str, label: &'static str| MenuAction::new(id, label);

        let mut items: Vec<MenuItem> = vec![
            action("context-actions", "Show Context Actions")
                .icon("ai-idea")
                .shortcut(keys("⌥↩", "Alt+Enter"))
                .disabled(!caps.actions || !writable)
                .into(),
            action("rename", "Rename…")
                .shortcut(keys("⇧F6", "Shift+F6"))
                .disabled(!caps.rename || !writable)
                .into(),
            action("refactor", "Refactor")
                .icon("wand")
                .disabled(!caps.actions || !writable)
                .submenu(
                    REFACTOR_KINDS
                        .iter()
                        .map(|(kind, label)| MenuAction::new(format!("kind:{kind}"), *label))
                        .collect(),
                )
                .into(),
            MenuItem::Separator,
            action(
                "ai-add",
                if selected {
                    "Add Selection to Chat"
                } else {
                    "Add File to Chat"
                },
            )
            .icon("comment-add")
            .into(),
            action("ai", "AI Actions")
                .icon("sparkles")
                .submenu(
                    AI_PROMPTS
                        .iter()
                        .map(|(id, label, _)| action(id, label))
                        .collect(),
                )
                .into(),
            MenuItem::Separator,
            action("cut", "Cut")
                .shortcut(keys("⌘X", "Ctrl+X"))
                .disabled(!selected || !writable)
                .into(),
            action("copy", "Copy")
                .shortcut(keys("⌘C", "Ctrl+C"))
                .disabled(!selected)
                .into(),
            action("paste", "Paste")
                .shortcut(keys("⌘V", "Ctrl+V"))
                .disabled(!clipboard || !writable)
                .into(),
            action("copy-special", "Copy Special")
                .submenu(vec![
                    action("copy-reference", "Copy Reference"),
                    action("copy-relative-path", "Copy Relative Path"),
                    action("copy-path", "Copy Absolute Path"),
                    action("copy-name", "Copy File Name"),
                ])
                .into(),
            action("compare-clipboard", "Compare with Clipboard")
                .icon("git-compare")
                .disabled(!clipboard)
                .into(),
            MenuItem::Separator,
            action("goto", "Go To")
                .icon("arrow-right-2")
                .submenu(vec![
                    action("definition", "Definition")
                        .shortcut("F12")
                        .disabled(!caps.definition),
                    action("declaration", "Declaration").disabled(!caps.navigation),
                    action("type-definition", "Type Definition").disabled(!caps.navigation),
                    action("implementation", "Implementation")
                        .shortcut(keys("⌘F12", "Ctrl+F12"))
                        .disabled(!caps.navigation),
                    action("usages", "Usages")
                        .shortcut(keys("⇧F12", "Shift+F12"))
                        .disabled(!caps.usages),
                    action("back", "Back").shortcut(keys("⌘⌥←", "Ctrl+Alt+Left")),
                    action("forward", "Forward").shortcut(keys("⌘⌥→", "Ctrl+Alt+Right")),
                ])
                .into(),
            action("folding", "Folding")
                .icon("fold-vertical")
                .disabled(!caps.folds)
                .submenu(vec![
                    action("fold", "Collapse").shortcut(keys("⌘⌥[", "Ctrl+Shift+[")),
                    action("unfold", "Expand").shortcut(keys("⌘⌥]", "Ctrl+Shift+]")),
                    action("fold-all", "Collapse All").shortcut(keys("⌘⌥⇧[", "Ctrl+Alt+Shift+[")),
                    action("unfold-all", "Expand All").shortcut(keys("⌘⌥⇧]", "Ctrl+Alt+Shift+]")),
                ])
                .into(),
            action("edit", "Edit")
                .icon("pencil-edit")
                .submenu(vec![
                    action("expand-selection", "Expand Selection").shortcut(keys("⌥↑", "Alt+Up")),
                    action("shrink-selection", "Shrink Selection").shortcut(keys("⌥↓", "Alt+Down")),
                    action("duplicate-line", "Duplicate Line").disabled(!writable),
                    action("move-line-up", "Move Line Up")
                        .shortcut(keys("⌥⇧↑", "Alt+Shift+Up"))
                        .disabled(!writable),
                    action("move-line-down", "Move Line Down")
                        .shortcut(keys("⌥⇧↓", "Alt+Shift+Down"))
                        .disabled(!writable),
                    action("column-selection", "Column Selection Mode")
                        .shortcut(keys("⇧⌘8", "Ctrl+Shift+8"))
                        .checked(input.column_selection()),
                    action("caret-above", "Add Caret Above"),
                    action("caret-below", "Add Caret Below"),
                ])
                .into(),
            MenuItem::Separator,
            action("comment", "Toggle Comment")
                .shortcut(keys("⌘/", "Ctrl+/"))
                .disabled(!caps.comment || !writable)
                .into(),
            action("format", "Reformat Code")
                .icon("wand")
                .shortcut(keys("⌥⇧F", "Alt+Shift+F"))
                .disabled(!writable)
                .into(),
        ];
        let run = self.run_menu_items(cx);
        if !run.is_empty() {
            items.push(MenuItem::Separator);
            items.extend(run);
        }
        let sql = self.sql_menu_items(sources, cx);
        if !sql.is_empty() {
            items.push(MenuItem::Separator);
            items.extend(sql);
        }
        items.extend([
            MenuItem::Separator,
            action("find", "Find…")
                .icon("search-01")
                .shortcut(keys("⌘F", "Ctrl+F"))
                .into(),
            action("replace", "Replace…")
                .shortcut(keys("⌘⌥F", "Ctrl+Alt+F"))
                .disabled(!writable)
                .into(),
            MenuItem::Separator,
            action("git", "Git")
                .icon("git-branch")
                .submenu(self.git_menu_items(cx))
                .into(),
            action("open-in", "Open In")
                .icon("external-link")
                .submenu(vec![
                    action("reveal", REVEAL_LABEL),
                    action("show-in-explorer", "Explorer"),
                ])
                .into(),
            MenuItem::Separator,
            action("wrap", "Soft-Wrap")
                .shortcut(keys("⌥Z", "Alt+Z"))
                .checked(input.word_wrap())
                .into(),
        ]);
        items
    }

    pub(super) fn open_code_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.visible || self.preview || self.document.is_none() {
            return;
        }
        let target = self.menu_target(cx);
        let sources = crate::database::send_targets(&self.database_dir, &self.cwd);
        let items = self.code_menu_items(&sources, cx);
        let menu = cx.new(|cx| {
            Menu::new("Code actions", items, window, cx)
                .style(MenuStyle::Code)
                .width(MENU_WIDTH)
                .submenu_width(SUBMENU_WIDTH)
        });
        let events = cx.subscribe_in(&menu, window, move |this, _, event, window, cx| {
            this.code_menu = None;
            if let MenuEvent::Picked(id, _) = event {
                this.run_code_menu(id, &target, &sources, window, cx);
            }
            cx.notify();
        });
        self.code_menu = Some(CodeMenu {
            menu,
            position,
            _events: events,
        });
        cx.notify();
    }

    fn run_code_menu(
        &mut self,
        id: &str,
        target: &super::selection::Target,
        sources: &[crate::database::Target],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let copy = |text: String, cx: &mut Context<Self>| {
            cx.write_to_clipboard(ClipboardItem::new_string(text))
        };
        if let Some((_, _, prompt)) = AI_PROMPTS.iter().find(|(key, ..)| *key == id) {
            cx.emit(FileEvent::AddToChat(format!(
                "{prompt} {}",
                target.reference
            )));
            return;
        }
        if self.sql_menu_pick(id, target, sources, window, cx)
            || self.run_menu_pick(id, window, cx)
            || self.git_menu_pick(id, window, cx)
        {
            return;
        }
        if let Some(kind) = id.strip_prefix("kind:") {
            let action = input::CodeActionsOfKind(kind.to_string().into());
            window.focus(&self.input.focus_handle(cx), cx);
            window.dispatch_action(Box::new(action), cx);
            return;
        }
        let action: Box<dyn Action> = match id {
            "ai-add" => {
                cx.emit(FileEvent::AddToChat(target.reference.clone()));
                return;
            }
            "copy-reference" => {
                return copy(format!("{}:{}", self.relative_path(), target.lines.0), cx);
            }
            "copy-relative-path" => return copy(self.relative_path(), cx),
            "copy-path" => return copy(self.path.clone(), cx),
            "copy-name" => {
                let name = std::path::Path::new(&self.path)
                    .file_name()
                    .map_or_else(|| self.path.clone(), |n| n.to_string_lossy().into_owned());
                return copy(name, cx);
            }
            "show-in-explorer" => {
                cx.emit(FileEvent::ShowInExplorer(self.path.clone()));
                return;
            }
            "reveal" => {
                let path = std::path::PathBuf::from(&self.path);
                cx.background_executor()
                    .spawn(async move {
                        if let Err(error) = monocode_core::files::reveal_path(&path) {
                            crate::review::log_error(&error);
                        }
                    })
                    .detach();
                return;
            }
            "cut" => Box::new(input::Cut),
            "copy" => Box::new(input::Copy),
            "paste" => Box::new(input::Paste),
            "context-actions" => Box::new(input::ShowContextActions),
            "rename" => Box::new(input::RenameSymbol),
            "definition" => Box::new(input::GoToDefinition),
            "declaration" => Box::new(input::GoToDeclaration),
            "type-definition" => Box::new(input::GoToTypeDefinition),
            "implementation" => Box::new(input::GoToImplementation),
            "usages" => Box::new(input::FindUsages),
            "fold" => Box::new(input::FoldCode),
            "unfold" => Box::new(input::UnfoldCode),
            "fold-all" => Box::new(input::FoldAll),
            "unfold-all" => Box::new(input::UnfoldAll),
            "comment" => Box::new(input::ToggleComment),
            "format" => Box::new(Format),
            "back" => Box::new(NavigateBack),
            "forward" => Box::new(NavigateForward),
            "find" => Box::new(Find),
            "replace" => Box::new(Replace),
            "compare-clipboard" => Box::new(CompareWithClipboard),
            "expand-selection" => Box::new(input::ExpandSelection),
            "shrink-selection" => Box::new(input::ShrinkSelection),
            "duplicate-line" => Box::new(input::DuplicateLine),
            "move-line-up" => Box::new(input::MoveLineUp),
            "move-line-down" => Box::new(input::MoveLineDown),
            "column-selection" => Box::new(input::ToggleColumnSelection),
            "caret-above" => Box::new(input::AddCaretAbove),
            "caret-below" => Box::new(input::AddCaretBelow),
            "wrap" => Box::new(ToggleWordWrap),
            _ => return,
        };
        // Through the editor's own focus, so the input's and this view's
        // handlers both see it, as the shortcut would reach them.
        window.focus(&self.input.focus_handle(cx), cx);
        window.dispatch_action(action, cx);
    }

    pub(super) fn code_menu_popover(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        self.code_menu.as_ref().map(|state| {
            menu_popover(&state.menu, Anchor::Point(state.position), cx).without_animation()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Modifiers, MouseButton, MouseDownEvent, TestAppContext, VisualTestContext, WindowHandle,
    };
    use std::{cell::RefCell, rc::Rc};

    struct Setup {
        window: WindowHandle<FileEditor>,
        events: Rc<RefCell<Vec<String>>>,
        sources: (String, String),
        _dirs: (tempfile::TempDir, tempfile::TempDir),
    }

    fn setup(cx: &mut TestAppContext) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("seed.sql");
        std::fs::write(
            &file,
            "insert into t values (1);\ninsert into t values (2);\n",
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        let (a, b) = crate::database::write_sources(db.path(), &["alpha", "beta"]);
        std::fs::write(
            db.path().join("state.json"),
            serde_json::json!({ "send_target": b }).to_string(),
        )
        .unwrap();
        cx.update(|cx| {
            cx.set_global(crate::settings::Preferences::default());
            crate::popover::bind(cx);
        });
        let path = file.to_string_lossy().into_owned();
        let database_dir = db.path().to_owned();
        let root = dir.path().to_string_lossy().into_owned();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                let git = cx.new(|cx| crate::git_status::ProjectGit::new(root, cx));
                cx.new(|cx| {
                    let mut editor = FileEditor::new(path, git, cx);
                    editor.database_dir = database_dir;
                    editor
                })
            })
            .unwrap()
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let log = events.clone();
        window
            .update(cx, |editor, _, cx| {
                cx.subscribe(&cx.entity(), move |_, _, event: &FileEvent, _| {
                    let text = match event {
                        FileEvent::AddToChat(text) => format!("chat {text}"),
                        FileEvent::SendToDatabase {
                            source,
                            text,
                            first_line,
                            last_line,
                        } => format!("db {source} {text} {first_line}-{last_line}"),
                        FileEvent::FileHistory { .. } => "history".into(),
                        FileEvent::ShowInExplorer(_) => "explorer".into(),
                        _ => return,
                    };
                    log.borrow_mut().push(text);
                })
                .detach();
                editor.set_visible(true, cx);
            })
            .unwrap();
        cx.run_until_parked();
        Setup {
            window,
            events,
            sources: (a, b),
            _dirs: (dir, db),
        }
    }

    fn ids(editor: &FileEditor, cx: &App) -> Vec<(String, String, bool)> {
        let sources = crate::database::send_targets(&editor.database_dir, &editor.cwd);
        editor
            .code_menu_items(&sources, cx)
            .into_iter()
            .filter_map(|item| match item {
                MenuItem::Action(a) => Some((a.id.to_string(), a.label.to_string(), a.disabled)),
                _ => None,
            })
            .collect()
    }

    #[gpui::test]
    fn the_menu_offers_the_file_without_a_selection_and_the_selection_with_one(
        cx: &mut TestAppContext,
    ) {
        let s = setup(cx);
        s.window
            .update(cx, |editor, _, cx| {
                let rows = ids(editor, cx);
                let add = rows.iter().find(|r| r.0 == "ai-add").unwrap();
                assert_eq!(add.1, "Add File to Chat");
                assert!(rows.iter().any(|r| r.0 == "cut" && r.2), "nothing to cut");
                assert!(
                    !rows.iter().any(|r| r.0 == "database"),
                    "no text to send to a data source"
                );
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(0..25, cx));
                let rows = ids(editor, cx);
                let add = rows.iter().find(|r| r.0 == "ai-add").unwrap();
                assert_eq!(add.1, "Add Selection to Chat");
                assert!(rows.iter().any(|r| r.0 == "cut" && !r.2));
                assert!(rows.iter().any(|r| r.0 == "database"));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_right_click_opens_the_code_menu_and_its_rows_reach_the_chat_and_database(
        cx: &mut TestAppContext,
    ) {
        let s = setup(cx);
        s.window
            .update(cx, |editor, window, cx| {
                window.focus(&editor.input.focus_handle(cx), cx);
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(0..25, cx));
            })
            .unwrap();
        cx.run_until_parked();
        let visual = &mut VisualTestContext::from_window(s.window.into(), cx);
        // Inside the selection, so it is kept for the menu.
        let at = s
            .window
            .read_with(visual, |editor, cx| {
                editor.input.read(cx).cursor_anchor().unwrap().origin
            })
            .unwrap()
            - gpui::point(gpui::px(20.), gpui::px(-4.));
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Right,
            position: at,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        visual.run_until_parked();
        assert!(
            s.window
                .read_with(visual, |editor, _| editor.code_menu.is_some())
                .unwrap(),
            "the file editor's menu opened, not the shared text menu"
        );
        let add = visual
            .debug_bounds("menu-action-ai-add")
            .expect("the menu shows Add Selection to Chat");
        visual.simulate_click(add.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(s.events.borrow().last().unwrap(), "chat @seed.sql (line 1)");

        // The data sources are a submenu, the one used last first.
        s.window
            .update(visual, |editor, window, cx| {
                editor.open_code_menu(at, window, cx)
            })
            .unwrap();
        visual.run_until_parked();
        let database = visual
            .debug_bounds("menu-action-database")
            .expect("a selection can be sent to a data source");
        visual.simulate_mouse_move(database.center(), None, Modifiers::default());
        visual.run_until_parked();
        let first = visual
            .debug_bounds("menu-action-db:0")
            .expect("the sources are listed");
        assert!(visual.debug_bounds("menu-action-db:1").is_some());
        visual.simulate_click(first.center(), Modifiers::default());
        visual.run_until_parked();
        let (_, beta) = &s.sources;
        assert_eq!(
            s.events.borrow().last().unwrap(),
            &format!("db {beta} insert into t values (1); 1-1")
        );
        assert!(
            s.window
                .read_with(visual, |editor, _| editor.code_menu.is_none())
                .unwrap()
        );
    }

    #[gpui::test]
    fn ai_and_file_rows_emit_their_requests_and_copy_rows_fill_the_clipboard(
        cx: &mut TestAppContext,
    ) {
        let s = setup(cx);
        s.window
            .update(cx, |editor, window, cx| {
                editor
                    .input
                    .update(cx, |input, cx| input.select_range(26..51, cx));
                let target = editor.menu_target(cx);
                assert_eq!(target.lines, (2, 2));
                for id in ["ai-explain", "history", "show-in-explorer"] {
                    editor.run_code_menu(id, &target, &[], window, cx);
                }
                editor.run_code_menu("copy-reference", &target, &[], window, cx);
                assert_eq!(
                    cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
                    Some("seed.sql:2")
                );
                editor.run_code_menu("copy-name", &target, &[], window, cx);
                assert_eq!(
                    cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
                    Some("seed.sql")
                );
            })
            .unwrap();
        assert_eq!(
            *s.events.borrow(),
            [
                "chat Explain what this code does: @seed.sql (line 2)",
                "history",
                "explorer"
            ]
        );
    }
}
