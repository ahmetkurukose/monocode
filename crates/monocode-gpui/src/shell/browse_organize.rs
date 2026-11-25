//! Organising Browser mode's own tabs (docs/native/modes.md §Organising):
//! folders among the pinned tabs, names the user gives tabs, and rows chosen
//! together with ⌘- and ⇧-click for one action on all of them. The model
//! (`socode_browser`) keeps them; these are the shell's verbs.
use super::modes::markdown_link;
use super::session_groups::{RenameHost, RenameTarget};
use super::*;
use crate::modes::BrowseSection;
use socode_browser::folders::FolderId;

/// Where a pinned tab or folder is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PinSpot {
    /// Before the pinned tab of this pane, in its folder.
    Before(i64),
    /// At the end of this folder.
    Into(u64),
    /// At the end of the pinned tabs.
    End,
}

/// What the icon picker gives an icon (docs/native/modes.md §Organising).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum IconTarget {
    /// The tab of this pane, or its Split View.
    Tab(i64),
    Folder(u64),
}

/// The icon picker's catalogues, as SOCode's: emoji, and symbols.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum IconCatalog {
    #[default]
    Emoji,
    Symbols,
}

/// GitHub's MIT-licensed Gemoji catalogue (`assets/emoji/LICENSE`): each
/// row is the emoji, its name, its category and more words that find it.
static EMOJI: std::sync::LazyLock<Vec<[String; 4]>> = std::sync::LazyLock::new(|| {
    serde_json::from_str(include_str!("../../assets/emoji/catalog.json")).unwrap_or_default()
});

/// The symbols offered: the value the model keeps (its portable symbol
/// name, `socode_browser::icons`) and the app's own icon drawn for it.
const SYMBOLS: &[(&str, &str)] = &[
    ("shape:circle", ""),
    ("ion:star", "star"),
    ("ion:flash", "zap"),
    ("ion:bulb", "ai-idea"),
    ("ion:rocket", "rocket"),
    ("ion:code", "code"),
    ("ion:terminal", "terminal"),
    ("ion:bug", "bug"),
    ("ion:git-branch", "git-branch"),
    ("ion:server", "database"),
    ("ion:grid", "dashboard"),
    ("ion:document", "file"),
    ("ion:folder", "folder-01"),
    ("ion:mail", "inbox"),
    ("ion:chatbubble-ellipses", "chat"),
    ("ion:people", "account-user"),
    ("ion:time", "clock"),
    ("ion:compass", "compass"),
    ("ion:globe", "globe"),
    ("ion:moon", "moon"),
    ("ion:sunny", "sun"),
    ("ion:cafe", "coffee"),
    ("ion:construct", "hammer"),
    ("ion:color-palette", "palette"),
    ("ion:pencil", "pencil"),
    ("ion:image", "image-01"),
    ("ion:key", "key"),
    ("ion:lock-closed", "lock"),
    ("ion:shield", "shield"),
    ("ion:settings", "settings"),
    ("ion:pin", "pin"),
    ("ion:send", "send"),
];

/// The emoji categories, in the catalogue's order, with the icon of each
/// one's button.
const EMOJI_CATEGORIES: &[(&str, &str)] = &[
    ("Smileys & Emotion", "smile"),
    ("Animals & Nature", "sun"),
    ("Food & Drink", "coffee"),
    ("Activities", "zap"),
    ("Travel & Places", "compass"),
    ("Objects", "package"),
    ("Symbols", "star"),
    ("Flags", "pin"),
];

/// One cell of the picker.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PickerEntry {
    pub value: String,
    pub label: String,
    pub category: String,
}

/// The picker's cells for `query`: emoji whose name, words or self match
/// it, or every symbol.
pub(super) fn picker_entries(catalog: IconCatalog, query: &str) -> Vec<PickerEntry> {
    let query = query.trim().to_lowercase();
    match catalog {
        IconCatalog::Emoji => (EMOJI.iter())
            .filter(|row| {
                query.is_empty()
                    || row[0] == query
                    || row[1].contains(&query)
                    || row[3].contains(&query)
            })
            .map(|row| PickerEntry {
                value: row[0].clone(),
                label: row[1].clone(),
                category: row[2].clone(),
            })
            .collect(),
        IconCatalog::Symbols => (SYMBOLS.iter())
            .map(|(value, _)| PickerEntry {
                value: value.to_string(),
                label: value.trim_start_matches("ion:").replace('-', " "),
                category: String::new(),
            })
            .collect(),
    }
}

/// The cells in rows of eight; with nothing typed, each emoji category
/// starts a row.
pub(super) fn picker_rows(catalog: IconCatalog, query: &str) -> Vec<Vec<PickerEntry>> {
    let grouped = catalog == IconCatalog::Emoji && query.trim().is_empty();
    let mut rows: Vec<Vec<PickerEntry>> = Vec::new();
    for entry in picker_entries(catalog, query) {
        let new_row = rows.last().is_none_or(|row| {
            row.len() == PICKER_COLUMNS || (grouped && row[0].category != entry.category)
        });
        if new_row {
            rows.push(Vec::new());
        }
        if let Some(row) = rows.last_mut() {
            row.push(entry);
        }
    }
    rows
}

/// A custom icon, drawn `side` points tall: an emoji, or a symbol.
pub(super) fn custom_icon_element(value: &str, side: f32) -> AnyElement {
    if value == "shape:circle" {
        return div()
            .size(px(side))
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(side * 0.72)).rounded_full().bg(content(0.8)))
            .into_any_element();
    }
    if let Some(name) = value.strip_prefix("ion:") {
        let path = (SYMBOLS.iter())
            .find(|(v, _)| v.strip_prefix("ion:") == Some(name))
            .map_or("star", |(_, path)| *path);
        return icon(path, side).text_color(content(0.8)).into_any_element();
    }
    div()
        .size(px(side + 2.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(side - 1.))
        .line_height(px(side + 2.))
        .child(value.to_string())
        .into_any_element()
}

const PICKER_WIDTH: f32 = 272.;
const PICKER_HEIGHT: f32 = 272.;
const PICKER_COLUMNS: usize = 8;
const PICKER_CELL: f32 = 31.;
const PICKER_EMOJI_ROW: f32 = 33.;

impl Shell {
    /// The custom icon of `target`, if it has one. A tab in a Split View
    /// has the split's.
    pub(super) fn browse_custom_icon(&self, target: IconTarget) -> Option<String> {
        let model = &self.workspace.browse.model;
        match target {
            IconTarget::Tab(pane) => model
                .item_icon(self.browse_model_tab(pane)?)
                .map(str::to_string),
            IconTarget::Folder(id) => self.workspace.browse.folder(id)?.custom_icon.clone(),
        }
    }

    /// `target` takes `icon`, or loses its own with `None`.
    pub(super) fn browse_set_icon(
        &mut self,
        target: IconTarget,
        icon: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        match target {
            IconTarget::Tab(pane) => {
                if let Some(tab) = self.browse_model_tab(pane) {
                    self.browse_edit(cx, |model, _| model.set_item_icon(tab, icon));
                }
            }
            IconTarget::Folder(id) => {
                let folder = FolderId::from_raw(id);
                self.browse_edit(cx, |model, _| model.set_folder_icon(folder, icon));
            }
        }
        self.persist(cx);
        cx.notify();
    }

    /// Change Icon…: the icon picker opens at the row's icon.
    pub(super) fn browse_change_icon(
        &mut self,
        target: IconTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browse_open_command_bar(super::command_bar::BarTarget::Icon(target), window, cx);
    }

    /// The icon picker, as SOCode's: Emoji and Icon catalogues, Remove
    /// while the target has an icon, a search for emoji, the grid, and the
    /// emoji categories. It sits under the row's icon (over it near the
    /// window's bottom). A cell gives its icon at once.
    pub(super) fn browse_icon_picker(
        &self,
        target: IconTarget,
        catalog: IconCatalog,
        input: &Entity<input::Input>,
        scroll: &gpui::ScrollHandle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let anchor =
            (self.browse_live.icon_anchors.borrow().get(&target).copied()).unwrap_or_else(|| {
                gpui::Bounds::new(gpui::point(px(17.), px(145.)), gpui::size(px(18.), px(18.)))
            });
        let emoji = catalog == IconCatalog::Emoji;
        let query = input.read(cx).content.to_string();
        let current = self.browse_custom_icon(target);
        let row_height = if emoji { PICKER_EMOJI_ROW } else { PICKER_CELL };
        let light = crate::theme::is_light();
        let selected_fill = if light { black(0.1) } else { white(0.2) };
        let hover_fill = if light { black(0.07) } else { white(0.16) };
        let mut index: usize = 0;
        let rows: Vec<AnyElement> = picker_rows(catalog, &query)
            .into_iter()
            .map(|row| {
                let cells: Vec<AnyElement> = row
                    .into_iter()
                    .map(|entry| {
                        let ix = index;
                        index += 1;
                        let chosen = current.as_deref() == Some(entry.value.as_str());
                        let value = entry.value.clone();
                        div()
                            .id(("browse-icon-cell", ix))
                            .debug_selector(move || format!("browse-icon-cell-{ix}"))
                            .role(Role::Button)
                            .aria_label(entry.label.clone())
                            .cursor_pointer()
                            .w(px(PICKER_CELL))
                            .h(px(row_height))
                            .flex_shrink_0()
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(chosen, |d| d.bg(selected_fill))
                            .hover(move |s| s.bg(hover_fill))
                            .child(if emoji {
                                div()
                                    .text_size(px(20.))
                                    .line_height(px(24.))
                                    .child(entry.value.clone())
                                    .into_any_element()
                            } else {
                                custom_icon_element(&entry.value, 15.)
                            })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let value = value.clone();
                                this.in_main_window(cx, move |this, _, cx| {
                                    this.browse_set_icon(target, Some(&value), cx)
                                });
                            }))
                            .into_any_element()
                    })
                    .collect();
                div()
                    .flex()
                    .flex_shrink_0()
                    .h(px(row_height))
                    .children(cells)
                    .into_any_element()
            })
            .collect();
        let tab_button = |id: &'static str, label: &'static str, which: IconCatalog| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(Role::Tab)
                .aria_selected(which == catalog)
                .cursor_pointer()
                .w(px(57.))
                .h_full()
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .when(which == catalog, |d| {
                    d.bg(gpui::rgb(0x1685ff)).text_color(gpui::white())
                })
                .child(label)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(bar) = &mut this.command_bar {
                        bar.icon_catalog = which;
                        bar.icon_scroll.set_offset(gpui::point(px(0.), px(0.)));
                    }
                    cx.notify();
                }))
        };
        let header = div()
            .absolute()
            .top(px(13.))
            .left_0()
            .w_full()
            .h(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .h(px(24.))
                    .rounded(px(5.))
                    .bg(content(0.1))
                    .child(tab_button("browse-icon-emoji", "Emoji", IconCatalog::Emoji))
                    .child(tab_button(
                        "browse-icon-symbols",
                        "Icon",
                        IconCatalog::Symbols,
                    )),
            )
            .when(current.is_some(), |d| {
                d.child(
                    div()
                        .id("browse-icon-remove")
                        .debug_selector(|| "browse-icon-remove".into())
                        .role(Role::Button)
                        .aria_label("Remove Icon")
                        .cursor_pointer()
                        .size(px(24.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(content(0.12)))
                        .child(icon("trash", 15.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.in_main_window(cx, move |this, _, cx| {
                                this.browse_set_icon(target, None, cx)
                            });
                        })),
                )
            });
        let search = emoji.then(|| {
            div()
                .absolute()
                .top(px(46.))
                .left(px(8.))
                .right(px(8.))
                .h(px(28.))
                .rounded(px(14.))
                .border_2()
                .border_color(gpui::rgb(0x327fa9))
                .bg(content(0.06))
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(7.))
                .child(icon("search", 12.).text_color(content(0.5)))
                .child(
                    div()
                        .debug_selector(|| "browse-icon-search".into())
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.5))
                        .child(input.clone()),
                )
        });
        let categories = emoji.then(|| {
            div()
                .absolute()
                .left(px(10.))
                .right(px(10.))
                .bottom(px(10.))
                .h(px(31.))
                .border_t_1()
                .border_color(content(0.12))
                .pt(px(4.))
                .flex()
                .items_center()
                .children(
                    EMOJI_CATEGORIES
                        .iter()
                        .enumerate()
                        .map(|(ix, (name, path))| {
                            let name: &'static str = name;
                            div()
                                .id(("browse-icon-category", ix))
                                .debug_selector(move || format!("browse-icon-category-{ix}"))
                                .role(Role::Button)
                                .aria_label(name)
                                .cursor_pointer()
                                .w(px(PICKER_CELL))
                                .h(px(24.))
                                .rounded(px(4.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(content(0.45))
                                .hover(|s| s.bg(content(0.1)))
                                .child(icon(path, 14.))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let Some(bar) = &this.command_bar else { return };
                                    let rows = picker_rows(IconCatalog::Emoji, "");
                                    let row = rows.iter().position(|r| r[0].category == name);
                                    let input = bar.input.clone();
                                    bar.icon_scroll.set_offset(gpui::point(
                                        px(0.),
                                        px(-(row.unwrap_or(0) as f32 * PICKER_EMOJI_ROW)),
                                    ));
                                    input.update(cx, |field, cx| field.set_text(String::new(), cx));
                                    cx.notify();
                                }))
                        }),
                )
        });
        let (grid_top, grid_height) = if emoji { (75., 154.) } else { (47., 217.) };
        let panel = div()
            .id("browse-icon-picker")
            .debug_selector(|| "browse-icon-picker".into())
            .key_context("CommandBar")
            .relative()
            .w(px(PICKER_WIDTH))
            .h(px(PICKER_HEIGHT))
            .rounded(px(20.))
            .bg(crate::theme::floating_fill())
            .border_1()
            .border_color(content(0.14))
            .shadow(vec![
                BoxShadow::new(px(0.), px(14.), gpui::rgba(0x00000052).into()).blur_radius(px(36.)),
            ])
            .text_size(px(12.))
            .text_color(content(0.92))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.in_main_window(cx, |this, window, cx| {
                        this.browse_close_command_bar(window, cx)
                    });
                }
            }))
            .child(header)
            .children(search)
            .child(
                div()
                    .id("browse-icon-grid")
                    .debug_selector(|| "browse-icon-grid".into())
                    .absolute()
                    .top(px(grid_top))
                    .left(px(12.))
                    .w(px(PICKER_CELL * PICKER_COLUMNS as f32))
                    .h(px(grid_height))
                    .overflow_y_scroll()
                    .track_scroll(scroll)
                    .flex()
                    .flex_col()
                    .children(rows),
            )
            .children(categories);
        gpui::anchored()
            .position(gpui::point(
                anchor.origin.x - px(3.),
                anchor.origin.y + anchor.size.height + px(12.),
            ))
            .snap_to_window_with_margin(px(8.))
            .child(panel)
            .into_any_element()
    }

    fn browse_url(&self, pane: i64, cx: &App) -> String {
        self.browse_tab(pane, cx)
            .map(|tab| tab.url)
            .unwrap_or_default()
    }

    // ---- Folders ----

    /// A new folder at `spot` holding `tabs`, its name ready to type. Tabs
    /// all on one site name it for the site, as Arc's.
    pub(super) fn browse_new_folder(
        &mut self,
        spot: PinSpot,
        tabs: &[i64],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        let hosts: Vec<String> = (tabs.iter())
            .map(|pane| super::modes_ui::host_of(&self.browse_url(*pane, cx)))
            .collect();
        let name = match hosts.first() {
            Some(host) if !host.is_empty() && hosts.iter().all(|h| h == host) => host.clone(),
            _ => "New Folder".to_string(),
        };
        let parent = match spot {
            PinSpot::Into(id) => Some(FolderId::from_raw(id)),
            PinSpot::Before(pane) => self
                .workspace
                .browse
                .parent_folder(pane)
                .map(FolderId::from_raw),
            PinSpot::End => None,
        };
        let tabs: Vec<_> = tabs
            .iter()
            .filter_map(|p| self.browse_model_tab(*p))
            .collect();
        let id = self.browse_edit(cx, |model, _| {
            let space = model.space().id;
            let id = model.create_folder(space, parent, &name)?;
            for tab in tabs {
                if !model.space().pinned.iter().any(|t| t.id == tab) {
                    model.move_tab(tab, space, socode_browser::Section::Pinned);
                }
                model.move_item_to_folder(tab, id);
            }
            Some(id.get())
        })?;
        self.start_rename_in(
            RenameTarget::BrowseFolder(id),
            RenameHost::Sidebar,
            window,
            cx,
        );
        Some(id)
    }

    /// A folder opens or closes.
    pub(super) fn browse_toggle_folder(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(open) = self.workspace.browse.folder(id).map(|f| !f.collapsed) else {
            return;
        };
        self.browse_edit(cx, |model, _| {
            model.set_folder_collapsed(FolderId::from_raw(id), open)
        });
    }

    /// The folder goes; its tabs and folders stay where it was.
    pub(super) fn browse_delete_folder(&mut self, id: u64, cx: &mut Context<Self>) {
        self.browse_edit(cx, |model, _| {
            let id = FolderId::from_raw(id);
            let space = model.space().id;
            let Some(folder) = model.folder(id).cloned() else {
                return;
            };
            // Its parent, if it has one, takes what it held.
            let parent = (model.space().folders.iter())
                .find(|f| {
                    f.items
                        .contains(&socode_browser::folders::PinnedItem::Folder(id))
                })
                .map(|f| f.id);
            for item in folder.items {
                match item {
                    socode_browser::folders::PinnedItem::Folder(child) => {
                        model.move_folder(child, space, parent);
                    }
                    other => {
                        if let Some(tab) = model.pinned_item_tab(space, other) {
                            match parent {
                                Some(parent) => model.move_item_to_folder(tab, parent),
                                None => model.move_item_to_pinned_root(tab, space),
                            };
                        }
                    }
                }
            }
            model.delete_empty_folder(id);
        });
    }

    /// A folder dragged to `spot`: into a folder, or beside a pinned tab in
    /// that tab's folder.
    pub(super) fn browse_move_folder(&mut self, id: u64, spot: PinSpot, cx: &mut Context<Self>) {
        let parent = match spot {
            PinSpot::Into(folder) => Some(folder),
            PinSpot::Before(pane) => self.workspace.browse.parent_folder(pane),
            PinSpot::End => None,
        };
        if parent == Some(id) {
            return;
        }
        self.browse_edit(cx, |model, _| {
            let space = model.space().id;
            model.move_folder(
                FolderId::from_raw(id),
                space,
                parent.map(FolderId::from_raw),
            )
        });
    }

    /// A tab of the user's own pinned at `spot` (dropped on a pinned row or
    /// a folder).
    pub(super) fn browse_pin_at(&mut self, pane: i64, spot: PinSpot, cx: &mut Context<Self>) {
        match spot {
            PinSpot::Before(target) => {
                self.browse_place(pane, BrowseSection::Pinned, Some(target), cx)
            }
            PinSpot::End => self.browse_place(pane, BrowseSection::Pinned, None, cx),
            PinSpot::Into(folder) => {
                let Some(tab) = self.browse_model_tab(pane) else {
                    return;
                };
                self.browse_edit(cx, |model, _| {
                    let space = model.space().id;
                    if !model.space().pinned.iter().any(|t| t.id == tab) {
                        model.move_tab(tab, space, socode_browser::Section::Pinned);
                    }
                    model.move_item_to_folder(tab, FolderId::from_raw(folder))
                });
            }
        }
    }

    /// The tabs go into the folder `folder`, or a new one where the first
    /// of them was.
    pub(super) fn browse_add_to_folder(
        &mut self,
        panes: &[i64],
        folder: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace.browse.selection.clear();
        match folder {
            Some(id) => {
                for pane in panes {
                    self.browse_pin_at(*pane, PinSpot::Into(id), cx);
                }
            }
            None => {
                let spot = (panes.first().copied())
                    .filter(|pane| self.workspace.browse.is_pinned(*pane))
                    .map_or(PinSpot::End, PinSpot::Before);
                self.browse_new_folder(spot, panes, window, cx);
            }
        }
    }

    /// Their links on the clipboard, one a line, as Markdown or plain.
    pub(super) fn browse_copy_links(&self, panes: &[i64], markdown: bool, cx: &mut Context<Self>) {
        let lines: Vec<String> = (panes.iter())
            .filter_map(|pane| self.browse_tab(*pane, cx))
            .filter(|tab| !tab.url.is_empty())
            .map(|tab| {
                if markdown {
                    format!("- {}", markdown_link(&tab.title, &tab.url))
                } else {
                    tab.url
                }
            })
            .collect();
        if !lines.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(lines.join("\n")));
        }
    }

    /// The address on the clipboard opens as a tab at the end of `folder`.
    fn browse_paste_into_folder(
        &mut self,
        folder: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(url) = (cx.read_from_clipboard())
            .and_then(|item| item.text())
            .and_then(|text| crate::browser::address(text.trim()))
        else {
            return;
        };
        let pane = self.browse_new_tab(Some(url), None, true, window, cx);
        self.browse_pin_at(pane, PinSpot::Into(folder), cx);
    }

    // ---- Names ----

    /// A tab's or folder's name becomes a field in its row.
    pub(super) fn browse_rename(
        &mut self,
        target: RenameTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_rename_in(target, RenameHost::Sidebar, window, cx);
    }

    /// A rename of a tab or folder of Browser mode ends: an empty name gives
    /// a tab its page's title back.
    pub(super) fn finish_browse_rename(
        &mut self,
        target: &RenameTarget,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        match target {
            RenameTarget::BrowseTab(pane) => {
                let Some(tab) = self.browse_model_tab(*pane) else {
                    return;
                };
                if name.is_empty() {
                    self.browse_reset_name(*pane, cx);
                    return;
                }
                if self.workspace.browse.name(*pane) == Some(name) {
                    return;
                }
                // Confirming the page's own title names nothing.
                let page_title =
                    (self.browser_pages.get(pane)).map(|(page, _)| page.read(cx).display_title());
                if page_title.as_deref() == Some(name)
                    && self.workspace.browse.name(*pane).is_none()
                {
                    return;
                }
                self.browse_edit(cx, |model, _| model.rename_tab(tab, name));
            }
            RenameTarget::BrowseFolder(id) => {
                if !name.is_empty() {
                    self.browse_edit(cx, |model, _| {
                        model.rename_folder(FolderId::from_raw(*id), name)
                    });
                }
            }
            _ => {}
        }
    }

    /// A tab's page title stands again over the name the user gave it.
    pub(super) fn browse_reset_name(&mut self, pane: i64, cx: &mut Context<Self>) {
        if let Some(tab) = self.browse_model_tab(pane) {
            self.browse_edit(cx, |model, _| model.reset_tab_name(tab));
        }
    }

    /// The field a tab or folder of Browser mode is renamed in, in place of
    /// its name.
    pub(super) fn browse_rename_field(
        &self,
        target: &RenameTarget,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let renaming = self.renaming.as_ref().filter(|r| r.target == *target)?;
        let mut outline = crate::theme::accent_color();
        outline.a = 0.45;
        Some(
            div()
                .id("browse-rename")
                .debug_selector(|| "browse-rename".into())
                .key_context("SessionRename")
                .on_action(cx.listener(
                    |this, _: &super::session_groups::CancelRename, window, cx| {
                        this.finish_rename(false, window, cx)
                    },
                ))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_click(|_, _, cx| cx.stop_propagation())
                .flex_1()
                .min_w_0()
                .h(px(24.))
                .px_1p5()
                .flex()
                .items_center()
                .rounded(px(6.))
                .bg(content(0.08))
                .border_1()
                .border_color(outline)
                .text_size(px(13.5))
                .text_color(content(1.))
                .child(renaming.input.clone())
                .into_any_element(),
        )
    }

    // ---- Rows chosen together ----

    /// A click on a row: ⌘ adds it to the rows chosen or takes it out, ⇧
    /// chooses the rows from the last one chosen to it, and a plain click
    /// shows it and lets the choice go.
    pub(super) fn browse_click(
        &mut self,
        pane: i64,
        modifiers: gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let browse = &mut self.workspace.browse;
        if !browse.owns(pane) || !(modifiers.platform || modifiers.shift) {
            browse.selection.clear();
            self.browse_select(pane, window, cx);
            return;
        }
        if browse.selection.is_empty()
            && let Some(active) = browse.active.filter(|a| browse.owns(*a) && *a != pane)
        {
            browse.selection.push(active);
        }
        if modifiers.shift {
            let order = browse.order();
            let anchor = (browse.selection.last().copied()).unwrap_or(pane);
            let (Some(a), Some(b)) = (
                order.iter().position(|id| *id == anchor),
                order.iter().position(|id| *id == pane),
            ) else {
                return;
            };
            let (from, to) = (a.min(b), a.max(b));
            for id in &order[from..=to] {
                if !browse.selection.contains(id) {
                    browse.selection.push(*id);
                }
            }
        } else if let Some(ix) = browse.selection.iter().position(|id| *id == pane) {
            browse.selection.remove(ix);
        } else {
            browse.selection.push(pane);
        }
        if browse.selection.len() < 2 {
            browse.selection.clear();
        }
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// The rows an action on `pane` acts on: the rows chosen when it is one
    /// of them, else it alone. In the sidebar's order.
    pub(super) fn browse_chosen(&self, pane: i64) -> Vec<i64> {
        let browse = &self.workspace.browse;
        if browse.selection.len() > 1 && browse.selection.contains(&pane) {
            (browse.order().into_iter())
                .filter(|id| browse.selection.contains(id))
                .collect()
        } else {
            vec![pane]
        }
    }

    // ---- Menus ----

    /// The menu of several rows chosen together, as SOCode's.
    pub(super) fn browse_selection_menu(
        &self,
        panes: &[i64],
    ) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        let browse = &self.workspace.browse;
        let muted = panes.iter().all(|pane| {
            self.workspace
                .browser_panes
                .get(pane)
                .is_some_and(|state| state.muted)
        });
        let today_only = panes
            .iter()
            .all(|p| browse.section(*p) == Some(BrowseSection::Today));
        let mut items: Vec<MenuItem> = vec![
            MenuAction::new("copy-links", "Copy Links").into(),
            MenuItem::Separator,
            MenuAction::new(
                if muted { "unmute" } else { "mute" },
                if muted { "Unmute" } else { "Mute" },
            )
            .into(),
            MenuItem::Separator,
            MenuAction::new("duplicate", "Duplicate").into(),
            MenuAction::new("move-to", "Move to")
                .submenu({
                    let mut items = vec![
                        MenuAction::new("move:pinned", "Pinned"),
                        MenuAction::new("move:today", "Today"),
                    ];
                    items.extend(
                        (browse.folder_paths().into_iter())
                            .map(|(id, path, _)| MenuAction::new(format!("folder:{id}"), path)),
                    );
                    items
                })
                .into(),
            MenuAction::new(
                "folder:new",
                format!("New Folder with {} Items", panes.len()),
            )
            .into(),
            MenuItem::Separator,
            MenuAction::new("close", "Archive Tabs").into(),
        ];
        if today_only {
            let today = browse.today();
            let last = panes
                .iter()
                .filter_map(|p| today.iter().position(|t| t == p))
                .max();
            items.push(
                MenuAction::new("close-below", "Archive Tabs Below")
                    .disabled(last.is_none_or(|ix| ix + 1 >= today.len()))
                    .into(),
            );
        }
        (
            format!("{} Tabs", panes.len()).into(),
            items,
            224.,
            MenuStyle::Native,
        )
    }

    pub(super) fn run_browse_selection_menu(
        &mut self,
        panes: Vec<i64>,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            "copy-links" => self.browse_copy_links(&panes, false, cx),
            "copy-markdown" => self.browse_copy_links(&panes, true, cx),
            "mute" | "unmute" => {
                for pane in &panes {
                    self.set_browser_muted(*pane, action == "mute", cx);
                }
            }
            "duplicate" => {
                for pane in &panes {
                    let url = self.browse_url(*pane, cx);
                    self.browse_new_tab(Some(url), None, false, window, cx);
                }
            }
            "pin" | "move:pinned" | "unpin" | "move:today" => {
                let section = if matches!(action, "pin" | "move:pinned") {
                    BrowseSection::Pinned
                } else {
                    BrowseSection::Today
                };
                for pane in panes.iter().rev() {
                    if self.workspace.browse.section(*pane) != Some(section) {
                        self.browse_place(*pane, section, None, cx);
                    }
                }
            }
            "close" => self.browse_close_chosen(panes.clone(), window, cx),
            "close-below" => {
                let today = self.workspace.browse.today();
                if let Some(last) = panes
                    .iter()
                    .filter_map(|p| today.iter().position(|t| t == p))
                    .max()
                {
                    let tabs: Vec<_> = (today[last + 1..].iter())
                        .filter_map(|p| self.browse_model_tab(*p))
                        .collect();
                    self.browse_edit(cx, |model, now| model.archive_sidebar_items(&tabs, now));
                }
            }
            _ => {
                if let Some(folder) = action.strip_prefix("folder:") {
                    let folder = folder.parse().ok();
                    self.browse_add_to_folder(&panes, folder, window, cx);
                }
            }
        }
        self.workspace.browse.selection.clear();
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// ⌘W on rows chosen together: each closes, or is put away when pinned.
    pub(super) fn browse_close_chosen(
        &mut self,
        panes: Vec<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace.browse.selection.clear();
        for pane in panes {
            self.browse_close(pane, window, cx);
        }
    }

    /// A folder's menu, as SOCode's.
    pub(super) fn browse_folder_menu(
        &self,
        id: u64,
    ) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        let empty = self.workspace.browse.folder_tabs(id).is_empty()
            && self
                .workspace
                .browse
                .folder(id)
                .is_none_or(|f| f.items.is_empty());
        let items: Vec<MenuItem> = vec![
            MenuAction::new("new-folder", "New Nested Folder…").into(),
            MenuItem::Separator,
            MenuAction::new("rename", "Rename…").into(),
            MenuAction::new("change-icon", "Change Icon…").into(),
        ];
        let mut items = items;
        if (self.workspace.browse.folder(id)).is_some_and(|f| f.custom_icon.is_some()) {
            items.push(MenuAction::new("reset-icon", "Reset Icon").into());
        }
        items.extend([
            MenuItem::Separator,
            MenuAction::new("duplicate", "Duplicate").into(),
            MenuAction::new("copy", "Copy Links")
                .submenu(vec![
                    MenuAction::new("copy-links", "Copy All Links").disabled(empty),
                    MenuAction::new("copy-markdown", "Copy All Links as Markdown").disabled(empty),
                ])
                .into(),
            MenuAction::new("paste", "Paste URL as New Tab").into(),
            MenuItem::Separator,
            if empty {
                MenuAction::new("delete", "Delete").into()
            } else {
                MenuAction::new("archive", "Archive Folder").into()
            },
        ]);
        ("Folder".into(), items, 236., MenuStyle::Native)
    }

    pub(super) fn run_browse_folder_menu(
        &mut self,
        id: u64,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tabs = self.workspace.browse.folder_tabs(id);
        let folder = FolderId::from_raw(id);
        match action {
            "rename" => self.browse_rename(RenameTarget::BrowseFolder(id), window, cx),
            "change-icon" => self.browse_change_icon(IconTarget::Folder(id), window, cx),
            "reset-icon" => self.browse_set_icon(IconTarget::Folder(id), None, cx),
            "new-folder" => {
                self.browse_new_folder(PinSpot::Into(id), &[], window, cx);
            }
            "duplicate" => {
                self.browse_edit(cx, |model, now| model.duplicate_folder(folder, now));
            }
            "copy-links" => self.browse_copy_links(&tabs, false, cx),
            "copy-markdown" => self.browse_copy_links(&tabs, true, cx),
            "paste" => self.browse_paste_into_folder(id, window, cx),
            "unload" => {
                for pane in tabs {
                    self.browse_unload(pane, window, cx);
                }
            }
            // The folder and its tabs go to the archive, together.
            "archive" => {
                self.browse_edit(cx, |model, now| model.archive_folder(folder, now));
            }
            "delete" => self.browse_delete_folder(id, cx),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::AppMode;
    use gpui::{TestAppContext, VisualTestContext};

    fn ids(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action(action) => Some(action.id.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_picker_finds_emoji_by_name_and_offers_valid_symbols() {
        for query in ["rocket", "🚀", "scientist", "compass"] {
            assert!(
                !picker_entries(IconCatalog::Emoji, query).is_empty(),
                "{query}"
            );
        }
        assert!(picker_entries(IconCatalog::Emoji, "no-such-emoji-12345").is_empty());
        let symbols = picker_entries(IconCatalog::Symbols, "ignored");
        assert_eq!(symbols.len(), SYMBOLS.len());
        for entry in &symbols {
            assert!(
                socode_browser::icons::valid_icon(&entry.value),
                "{}",
                entry.value
            );
        }
        // Rows of eight; with nothing typed each category starts a row.
        let rows = picker_rows(IconCatalog::Emoji, "");
        assert!(rows.iter().all(|row| row.len() <= PICKER_COLUMNS));
        assert!(
            rows.windows(2)
                .all(|pair| pair[0][0].category == pair[1][0].category
                    || pair[0].last().unwrap().category != pair[1][0].category)
        );
    }

    #[gpui::test]
    fn change_icon_gives_tabs_and_folders_an_icon_drawn_in_their_rows(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = super::super::tests::window(cx);
        let (pane, folder) = window
            .update(cx, |shell, window, cx| {
                shell.workspace.classic_layout = true;
                shell.set_mode(AppMode::Browser, window, cx);
                let pane =
                    shell.browse_new_tab(Some("https://a.test/".into()), None, true, window, cx);
                shell.browse_toggle_pin(pane, cx);
                let folder = shell
                    .browse_new_folder(PinSpot::End, &[], window, cx)
                    .unwrap();
                shell.renaming = None;
                (pane, folder)
            })
            .unwrap();
        window
            .update(cx, |shell, window, cx| {
                // The tab's menu offers it; Reset Icon only once it has one.
                let (_, items, _, _) = shell.browse_tab_menu(pane, cx);
                assert!(ids(&items).contains(&"change-icon".to_string()));
                assert!(!ids(&items).contains(&"reset-icon".to_string()));
                shell.run_browse_tab_menu(pane, "change-icon", window, cx);
                let bar = shell.command_bar.as_ref().expect("the picker");
                assert_eq!(
                    bar.target,
                    super::super::command_bar::BarTarget::Icon(IconTarget::Tab(pane))
                );
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(visual.debug_bounds("browse-icon-picker").is_some());
        let cell = visual.debug_bounds("browse-icon-cell-0").expect("a cell");
        visual.simulate_click(cell.center(), Default::default());
        visual.run_until_parked();
        window
            .update(cx, |shell, window, cx| {
                let first = picker_entries(IconCatalog::Emoji, "")[0].value.clone();
                assert_eq!(shell.browse_custom_icon(IconTarget::Tab(pane)), Some(first));
                let (_, items, _, _) = shell.browse_tab_menu(pane, cx);
                assert!(ids(&items).contains(&"reset-icon".to_string()));
                shell.run_browse_tab_menu(pane, "reset-icon", window, cx);
                assert_eq!(shell.browse_custom_icon(IconTarget::Tab(pane)), None);
                // A folder takes a symbol; Enter in the picker gives the first
                // emoji found.
                shell.browse_close_command_bar(window, cx);
                let (_, items, _, _) = shell.browse_folder_menu(folder);
                assert!(ids(&items).contains(&"change-icon".to_string()));
                shell.browse_set_icon(IconTarget::Folder(folder), Some("ion:rocket"), cx);
                assert_eq!(
                    shell
                        .browse_custom_icon(IconTarget::Folder(folder))
                        .as_deref(),
                    Some("ion:rocket")
                );
                let (_, items, _, _) = shell.browse_folder_menu(folder);
                assert!(ids(&items).contains(&"reset-icon".to_string()));
                shell.run_browse_folder_menu(folder, "change-icon", window, cx);
                let input = shell.command_bar.as_ref().unwrap().input.clone();
                input.update(cx, |field, cx| field.set_text("rocket".into(), cx));
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(cx, |shell, window, cx| {
                shell.browse_command_bar_submit_for_test(window, cx);
                assert!(shell.command_bar.is_none());
                assert_eq!(
                    shell
                        .browse_custom_icon(IconTarget::Folder(folder))
                        .as_deref(),
                    Some("🚀")
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn rows_new_to_the_sidebar_fade_in(cx: &mut TestAppContext) {
        let (window, _, _) = super::super::tests::window(cx);
        let first = window
            .update(cx, |shell, window, cx| {
                shell.workspace.classic_layout = true;
                shell.set_mode(AppMode::Browser, window, cx);
                shell.browse_new_tab(Some("https://a.test/".into()), None, true, window, cx)
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let second = window
            .update(cx, |shell, window, cx| {
                shell.browse_new_tab(Some("https://b.test/".into()), None, true, window, cx)
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                let fresh = &shell.browse_live.fresh_rows;
                assert!(fresh.contains_key(&IconTarget::Tab(second)), "{fresh:?}");
                assert!(!fresh.contains_key(&IconTarget::Tab(first)), "{fresh:?}");
            })
            .unwrap();
    }
}
