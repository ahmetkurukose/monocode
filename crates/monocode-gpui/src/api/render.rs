//! Drawing the API view.

use super::*;

/// A bordered box around an input.
fn field_box(input: Entity<Input>) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .min_h(px(30.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(content(0.1))
        .bg(content(0.04))
        .child(input)
}

fn badge(method: &str) -> AnyElement {
    let hex = method_color(method);
    let short = match method {
        "DELETE" => "DEL",
        "OPTIONS" => "OPT",
        "PATCH" => "PAT",
        other => other,
    };
    div()
        .w(px(30.))
        .flex_shrink_0()
        .text_size(px(9.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(hex))
        .child(short.to_owned())
        .into_any_element()
}

fn toggle(id: impl Into<gpui::ElementId>, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(if on { "Disable" } else { "Enable" })
        .w(px(30.))
        .h(px(18.))
        .flex_shrink_0()
        .rounded_full()
        .cursor_pointer()
        .flex()
        .items_center()
        .px(px(2.))
        .bg(if on { accent_color() } else { content(0.18) })
        .when(on, |d| d.justify_end())
        .child(div().size(px(14.)).rounded_full().bg(gpui::white()))
}

fn small_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    glyph: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .size(px(24.))
        .flex_shrink_0()
        .rounded(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(content(0.4))
        .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
        .child(icon(glyph, 13.))
}

/// A tab-like label in a strip.
fn strip_label(
    id: impl Into<gpui::ElementId>,
    label: String,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::Tab)
        .aria_label(label.clone())
        .aria_selected(on)
        .h(px(26.))
        .px(px(10.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(px(12.5))
        .text_color(content(if on { 1. } else { 0.55 }))
        .when(on, |d| d.bg(content(0.1)))
        .hover(|s| s.bg(content(0.07)))
        .child(label)
}

fn count_label(name: &str, count: usize) -> String {
    if count > 0 {
        format!("{name} {count}")
    } else {
        name.to_owned()
    }
}

/// The test scripts' results: a summary, then each test with why it failed.
fn tests_view(id: u64, tests: &[api::TestResult]) -> AnyElement {
    let passed = tests.iter().filter(|t| t.passed).count();
    div()
        .id(("api-tests", id as usize))
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(16.))
        .py(px(8.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(if passed == tests.len() {
                    0x4ade80
                } else {
                    0xf87171
                }))
                .child(if tests.is_empty() {
                    "No tests ran.".to_owned()
                } else {
                    format!("{passed} of {} passed", tests.len())
                }),
        )
        .children(tests.iter().map(|test| {
            let (glyph, hex) = if test.skipped {
                ("minus-sign", 0x9ca3af)
            } else if test.passed {
                ("check", 0x4ade80)
            } else {
                ("close", 0xf87171)
            };
            div()
                .flex()
                .items_start()
                .gap(px(8.))
                .child(icon(glyph, 13.).text_color(rgb(hex)).mt(px(2.)))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(content(if test.passed { 0.8 } else { 1. }))
                                .child(test.name.clone()),
                        )
                        .when_some(test.error.clone(), |d, error| {
                            d.child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(rgb(0xf87171))
                                    .child(error),
                            )
                        }),
                )
        }))
        .into_any_element()
}

/// What the scripts wrote with `console.log`.
fn console_view(id: u64, logs: &[String]) -> AnyElement {
    div()
        .id(("api-console", id as usize))
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(16.))
        .py(px(8.))
        .flex()
        .flex_col()
        .gap(px(2.))
        .text_size(px(12.))
        .font_family("Menlo")
        .when(logs.is_empty(), |d| {
            d.text_color(content(0.45)).child("Nothing was logged.")
        })
        .children(logs.iter().map(|line| {
            let hex = if line.starts_with("error:") {
                Some(0xf87171)
            } else if line.starts_with("warning:") {
                Some(0xfb923c)
            } else {
                None
            };
            div()
                .text_color(hex.map_or(content(0.8), |h| rgb(h).into()))
                .child(line.clone())
        }))
        .into_any_element()
}

impl ApiView {
    // ---- the tree ----

    fn tree_row(&mut self, ix: usize, row: &TreeRow, cx: &mut Context<Self>) -> AnyElement {
        match row {
            TreeRow::Note(depth, text) => div()
                .h(px(TREE_ROW))
                .pl(px(26. + *depth as f32 * 14.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .text_color(content(0.4))
                .child(text.clone())
                .into_any_element(),
            TreeRow::Collection(ci) => {
                let Some(loaded) = self.collections.get(*ci) else {
                    return div().into_any_element();
                };
                let key = Self::collection_key(&loaded.file);
                let open = !self.collapsed.contains(&key);
                let name = loaded.collection.name();
                let detail = if self.scope.is_none() && self.projects.len() > 1 {
                    Some(project_name(&loaded.project))
                } else {
                    None
                };
                let target = Target {
                    file: loaded.file.clone(),
                    path: Vec::new(),
                };
                let menu_target = target.clone();
                div()
                    .id(("api-tree-row", ix))
                    .role(Role::TreeItem)
                    .aria_label(name.clone())
                    .h(px(TREE_ROW))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(content(0.9))
                    .hover(|s| s.bg(content(0.07)))
                    .child(
                        icon(
                            if open {
                                "chevron-down"
                            } else {
                                "chevron-right"
                            },
                            12.,
                        )
                        .text_color(content(0.4)),
                    )
                    .child(icon("api", 14.).text_color(content(0.55)))
                    .child(div().min_w_0().flex_1().truncate().child(name))
                    .when_some(detail, |d, text| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .max_w(px(110.))
                                .truncate()
                                .text_size(px(11.))
                                .text_color(content(0.35))
                                .child(text),
                        )
                    })
                    .child(
                        small_button(("api-row-add", ix), "Add request or folder", "plus")
                            .size(px(20.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let target = menu_target.clone();
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        this.open_node_menu(
                                            target.clone(),
                                            true,
                                            event.position,
                                            window,
                                            cx,
                                        );
                                        cx.stop_propagation();
                                    }
                                }),
                            )
                            .on_click(cx.listener(|_, _, _, cx| cx.stop_propagation())),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            this.open_node_menu(
                                menu_target.clone(),
                                true,
                                event.position,
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle(key.clone(), cx);
                    }))
                    .into_any_element()
            }
            TreeRow::Entry(ci, entry) => {
                let Some(loaded) = self.collections.get(*ci) else {
                    return div().into_any_element();
                };
                let file = loaded.file.clone();
                let target = Target {
                    file: file.clone(),
                    path: entry.path.clone(),
                };
                let selected = self
                    .active_tab()
                    .is_some_and(|tab| tab.file == file && tab.path == entry.path);
                let open = !self
                    .collapsed
                    .contains(&Self::folder_key(&file, &entry.path));
                let folder = entry.folder;
                let key = Self::folder_key(&file, &entry.path);
                let menu_target = target.clone();
                div()
                    .id(("api-tree-row", ix))
                    .role(Role::TreeItem)
                    .aria_label(entry.name.clone())
                    .h(px(TREE_ROW))
                    .pl(px(8. + (entry.depth + 1) as f32 * 14.))
                    .pr(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .text_color(content(0.85))
                    .when(selected, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.07)))
                    .child(div().w(px(12.)).flex_shrink_0().when(folder, |d| {
                        d.child(
                            icon(
                                if open {
                                    "chevron-down"
                                } else {
                                    "chevron-right"
                                },
                                12.,
                            )
                            .text_color(content(0.4)),
                        )
                    }))
                    .child(if folder {
                        icon("folder", 14.)
                            .text_color(content(0.5))
                            .into_any_element()
                    } else {
                        badge(entry.method.as_deref().unwrap_or("GET"))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(entry.name.clone()),
                    )
                    .when(folder, |d| {
                        d.child(
                            small_button(("api-row-add", ix), "Add request or folder", "plus")
                                .size(px(20.))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener({
                                        let target = menu_target.clone();
                                        move |this, event: &gpui::MouseDownEvent, window, cx| {
                                            this.open_node_menu(
                                                target.clone(),
                                                true,
                                                event.position,
                                                window,
                                                cx,
                                            );
                                            cx.stop_propagation();
                                        }
                                    }),
                                )
                                .on_click(cx.listener(|_, _, _, cx| cx.stop_propagation())),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            this.open_node_menu(
                                menu_target.clone(),
                                folder,
                                event.position,
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if folder {
                            this.toggle(key.clone(), cx);
                        } else {
                            this.open_request(&target.file, target.path.clone(), window, cx);
                        }
                    }))
                    .into_any_element()
            }
        }
    }

    fn tree_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let rows = Rc::new(self.rows(cx));
        let shown = rows.clone();
        let entity = cx.entity();
        let empty = self.collections.is_empty();
        let errors = self.load_errors.clone();
        div()
            .id("api-tree")
            .relative()
            .w(px(self.tree_width))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(content(0.07))
            .child(
                div()
                    .h(px(36.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(0.5))
                            .child("COLLECTIONS"),
                    )
                    .child(
                        icon_button("api-add", "New collection or import", "plus")
                            .size(px(24.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                                    let mut items: Vec<MenuItem> = Vec::new();
                                    if !this.collections.is_empty() {
                                        items
                                            .push(MenuAction::new("request", "New request").into());
                                        items.push(MenuAction::new("folder", "New folder").into());
                                        items.push(MenuItem::Separator);
                                    }
                                    items.push(
                                        MenuAction::new("collection", "New collection…").into(),
                                    );
                                    items.push(
                                        MenuAction::new("import", "Import Postman files…").into(),
                                    );
                                    this.open_menu(
                                        MenuKind::Add,
                                        items,
                                        "Add",
                                        event.position,
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }),
                            ),
                    )
                    .child(
                        icon_button("api-collapse", "Collapse all folders", "fold-vertical")
                            .size(px(24.))
                            .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
                    )
                    .child(
                        icon_button("api-expand", "Expand all", "unfold-vertical")
                            .size(px(24.))
                            .on_click(cx.listener(|this, _, _, cx| this.expand_all(cx))),
                    )
                    .child(
                        icon_button("api-reload", "Reload from disk", "refresh-cw")
                            .size(px(24.))
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .child(
                div()
                    .px(px(10.))
                    .pb(px(8.))
                    .child(field_box(self.filter.clone())),
            )
            .child(if empty {
                div()
                    .flex_1()
                    .px(px(14.))
                    .py(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .text_color(content(0.45))
                    .child("No collections in this project yet.")
                    .child(
                        modal::primary_button("api-empty-new", "New collection").on_click(
                            cx.listener(|this, _, window, cx| {
                                this.start_new_collection(window, cx)
                            }),
                        ),
                    )
                    .child(
                        crate::project_settings::button("api-empty-import", "Import from Postman")
                            .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
                    )
                    .children(errors.into_iter().map(|error| {
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(0xfb923c))
                            .child(error)
                    }))
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .children(errors.into_iter().map(|error| {
                        div()
                            .px(px(12.))
                            .py(px(4.))
                            .text_size(px(11.))
                            .text_color(rgb(0xfb923c))
                            .child(error)
                    }))
                    .child(
                        uniform_list("api-tree-rows", rows.len(), move |range, _, cx| {
                            entity.update(cx, |this, cx| {
                                let rows = shown.clone();
                                range
                                    .filter_map(|ix| rows.get(ix).cloned().map(|row| (ix, row)))
                                    .map(|(ix, row)| this.tree_row(ix, &row, cx))
                                    .collect::<Vec<_>>()
                            })
                        })
                        .flex_1()
                        .min_h_0(),
                    )
                    .into_any_element()
            })
            .child(
                div()
                    .id("api-tree-resize")
                    .role(Role::Splitter)
                    .aria_label("Resize collections")
                    .absolute()
                    .right(px(-3.))
                    .top_0()
                    .bottom_0()
                    .w(px(6.))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                            this.resize =
                                Some(Resize::Tree(f32::from(event.position.x), this.tree_width));
                            cx.stop_propagation();
                        }),
                    ),
            )
            .into_any_element()
    }

    // ---- tabs ----

    fn tab_strip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tabs: Vec<(u64, String, String, bool)> = self
            .tabs
            .iter()
            .map(|t| {
                (
                    t.id,
                    t.draft.method.clone(),
                    t.draft.name.clone(),
                    t.dirty(),
                )
            })
            .collect();
        let active = self.active;
        let dirty_count = tabs.iter().filter(|t| t.3).count();
        let has_tabs = !tabs.is_empty();
        let project = self.current_project();
        let environment = self
            .selected_environment()
            .map(|e| e.environment.name())
            .unwrap_or_else(|| "No environment".to_owned());
        div()
            .h(px(36.))
            .flex_shrink_0()
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(2.))
            .border_b_1()
            .border_color(content(0.07))
            .child(
                div()
                    .id("api-tabs")
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .overflow_x_scroll()
                    .children(tabs.into_iter().map(|(id, method, name, dirty)| {
                        let on = active == Some(id);
                        div()
                            .id(("api-tab", id as usize))
                            .role(Role::Tab)
                            .aria_label(name.clone())
                            .aria_selected(on)
                            .group("api-tab")
                            .h(px(26.))
                            .max_w(px(200.))
                            .flex_shrink_0()
                            .pl(px(8.))
                            .pr(px(4.))
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(content(if on { 1. } else { 0.55 }))
                            .when(on, |d| d.bg(content(0.1)))
                            .hover(|s| s.bg(content(0.07)))
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(method_color(&method)))
                                    .child(method),
                            )
                            .child(div().min_w_0().truncate().child(name))
                            .child(
                                div()
                                    .id(("api-tab-close", id as usize))
                                    .role(Role::Button)
                                    .aria_label("Close request")
                                    .size(px(18.))
                                    .flex_shrink_0()
                                    .rounded(px(4.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(content(0.4))
                                    .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                                    .child(if dirty {
                                        div()
                                            .size(px(7.))
                                            .rounded_full()
                                            .bg(accent_color())
                                            .into_any_element()
                                    } else {
                                        icon("close", 10.).into_any_element()
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.close_tab(id, window, cx);
                                        cx.stop_propagation();
                                    })),
                            )
                            // A middle click closes it, as in a browser.
                            .on_mouse_down(
                                MouseButton::Middle,
                                cx.listener(move |this, _, window, cx| {
                                    this.close_tab(id, window, cx);
                                    cx.stop_propagation();
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        this.open_tab_menu(id, event.position, window, cx);
                                        cx.stop_propagation();
                                    },
                                ),
                            )
                            .on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.activate(id, window, cx)
                                }),
                            )
                    })),
            )
            .when(dirty_count > 1, |d| {
                d.child(
                    crate::project_settings::button("api-save-all", "Save all")
                        .h(px(26.))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.save_all(cx);
                        })),
                )
            })
            .when(has_tabs, |d| {
                d.child(
                    icon_button(
                        "api-tabs-menu",
                        "Tabs: close several, reopen, jump",
                        "more-horizontal",
                    )
                    .size(px(26.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                            this.open_tabs_menu(event.position, window, cx);
                            cx.stop_propagation();
                        }),
                    ),
                )
            })
            .child(
                crate::project_settings::button("api-environment", environment)
                    .h(px(26.))
                    .child(icon("chevron-down", 11.).text_color(content(0.4)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            let Some(project) = project.clone() else {
                                return;
                            };
                            let mut items: Vec<MenuItem> =
                                vec![MenuAction::new("none", "No environment").into()];
                            for env in this.environments.iter().filter(|e| e.project == project) {
                                items.push(
                                    MenuAction::new(
                                        env.file.to_string_lossy().into_owned(),
                                        env.environment.name(),
                                    )
                                    .checked(this.environment.get(&project) == Some(&env.file))
                                    .into(),
                                );
                            }
                            items.push(MenuItem::Separator);
                            items.push(MenuAction::new("manage", "Edit environments…").into());
                            this.open_menu(
                                MenuKind::Environment(project),
                                items,
                                "Environments",
                                event.position,
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }),
                    ),
            )
            .into_any_element()
    }

    // ---- the request ----

    fn breadcrumb(&self, tab: &RequestTab) -> String {
        let Some(loaded) = self.collection(&tab.file) else {
            return String::new();
        };
        let mut parts = vec![loaded.collection.name()];
        for depth in 1..tab.path.len() {
            if let Some(name) = loaded.collection.item_name(&tab.path[..depth]) {
                parts.push(name);
            }
        }
        parts.join("  ›  ")
    }

    fn request_bar(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab(id) else {
            return div().into_any_element();
        };
        let method = tab.draft.method.clone();
        let sending = matches!(tab.exchange, Exchange::Sending);
        let dirty = tab.dirty();
        let breadcrumb = self.breadcrumb(tab);
        let name = tab.draft.name.clone();
        let url = tab.url.clone();
        let prepared = self.prepare_tab(tab);
        // A pre-request script can still define what is missing.
        let scripted = self
            .scripts_for(tab, "prerequest")
            .iter()
            .any(|s| !s.trim().is_empty());
        let missing = if scripted {
            Vec::new()
        } else {
            prepared.missing.clone()
        };
        let empty_url = (!prepared.url.contains("://") && !prepared.empty.is_empty())
            .then(|| prepared.empty.clone());
        let notes = prepared.notes.clone();
        div()
            .flex_shrink_0()
            .px(px(14.))
            .pt(px(10.))
            .pb(px(6.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(12.5))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(content(0.5))
                                    .child(format!("{breadcrumb}  ›")),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(content(1.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(name),
                            ),
                    )
                    .child(
                        small_button(
                            ("api-ask-agent", id as usize),
                            "Ask an agent about this request",
                            "sparkles",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.ask_agent(id, cx))),
                    )
                    .child(
                        small_button(("api-copy-curl", id as usize), "Copy as cURL", "terminal")
                            .on_click(cx.listener(move |this, _, _, cx| this.copy_curl(id, cx))),
                    )
                    .child(
                        small_button(("api-revert", id as usize), "Discard changes", "undo")
                            .when(!dirty, |d| d.opacity(0.35))
                            .on_click(cx.listener(move |this, _, _, cx| this.revert(id, cx))),
                    )
                    .child(
                        crate::project_settings::button(("api-save", id as usize), "Save")
                            .h(px(24.))
                            .when(!dirty, |d| d.opacity(0.4))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.save(id, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id(("api-method", id as usize))
                            .role(Role::Button)
                            .aria_label("Request method")
                            .w(px(104.))
                            .h(px(34.))
                            .flex_shrink_0()
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(content(0.1))
                            .bg(content(0.04))
                            .cursor_pointer()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(method_color(&method)))
                            .hover(|s| s.bg(content(0.07)))
                            .child(method)
                            .child(icon("chevron-down", 11.).text_color(content(0.4)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        let items: Vec<MenuItem> = METHODS
                                            .iter()
                                            .map(|m| MenuAction::new(*m, *m).into())
                                            .collect();
                                        this.open_menu(
                                            MenuKind::Method(id),
                                            items,
                                            "Method",
                                            event.position,
                                            window,
                                            cx,
                                        );
                                        cx.stop_propagation();
                                    },
                                ),
                            ),
                    )
                    .child(field_box(url).min_h(px(34.)).text_size(px(13.)))
                    .child(if sending {
                        crate::project_settings::button(("api-cancel", id as usize), "Cancel")
                            .h(px(34.))
                            .w(px(84.))
                            .on_click(cx.listener(move |this, _, _, cx| this.cancel(id, cx)))
                            .into_any_element()
                    } else {
                        modal::primary_button(("api-send", id as usize), "Send")
                            .h(px(34.))
                            .w(px(84.))
                            .on_click(cx.listener(move |this, _, _, cx| this.send(id, cx)))
                            .into_any_element()
                    }),
            )
            .when(!missing.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(rgb(0xfb923c))
                        .child(format!(
                            "No value for {}. Pick an environment or define the variable.",
                            missing
                                .iter()
                                .map(|name| format!("{{{{{name}}}}}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                )
            })
            .when_some(empty_url, |d, names| {
                d.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(rgb(0xfb923c))
                        .child(format!(
                            "{} is empty, so the URL has no host. Give it a value in the \
                             environment or the collection variables.",
                            names
                                .iter()
                                .map(|name| format!("{{{{{name}}}}}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                )
            })
            .children(notes.into_iter().map(|note| {
                div()
                    .text_size(px(11.5))
                    .text_color(content(0.5))
                    .child(note)
            }))
            .into_any_element()
    }

    fn section_strip(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab(id) else {
            return div().into_any_element();
        };
        let enabled = |rows: &[KeyValue]| rows.iter().filter(|r| !r.disabled).count();
        let sections = [
            (
                Section::Path,
                count_label("Path", tab.draft.path_vars.len()),
            ),
            (
                Section::Query,
                count_label("Query", enabled(&tab.draft.query)),
            ),
            (
                Section::Headers,
                count_label("Headers", enabled(&tab.draft.headers)),
            ),
            (Section::Body, "Body".to_owned()),
            (Section::Auth, "Authentication".to_owned()),
            (
                Section::Scripts,
                if tab.draft.pre_request.trim().is_empty() && tab.draft.tests.trim().is_empty() {
                    "Scripts".to_owned()
                } else {
                    "Scripts •".to_owned()
                },
            ),
            (Section::Notes, "Notes".to_owned()),
        ];
        let current = tab.section;
        div()
            .flex_shrink_0()
            .px(px(10.))
            .pb(px(6.))
            .flex()
            .items_center()
            .gap(px(2.))
            .children(
                sections
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (section, label))| {
                        strip_label(
                            ("api-section", id as usize * 8 + ix),
                            label,
                            current == section,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(tab) = this.tab_mut(id) {
                                tab.section = section;
                            }
                            cx.notify();
                        }))
                    }),
            )
            .into_any_element()
    }

    fn rows_editor(&mut self, id: u64, table: Table, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab(id) else {
            return div().into_any_element();
        };
        let fixed = table == Table::Path;
        let rows: Vec<(Entity<Input>, Entity<Input>, bool, bool)> = tab
            .rows(table)
            .iter()
            .map(|r| (r.key.clone(), r.value.clone(), r.disabled, r.file))
            .collect();
        let empty = rows.is_empty();
        let label = match table {
            Table::Query => "Add query parameter",
            Table::Headers => "Add header",
            Table::UrlEncoded => "Add field",
            Table::Form => "Add field",
            Table::Path => "",
        };
        let base = id as usize * 64 + table as usize * 8;
        div()
            .id(("api-rows", base))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(14.))
            .py(px(4.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .when(empty && fixed, |d| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(content(0.45))
                        .child("Put :name in the URL path to add a path variable."),
                )
            })
            .children(
                rows.into_iter()
                    .enumerate()
                    .map(|(ix, (key, value, off, file))| {
                        let row_id = base * 1000 + ix;
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .when(off, |d| d.opacity(0.55))
                            .child(field_box(key).when(fixed, |d| d.opacity(0.7)))
                            .child(field_box(value).when(table == Table::Form && file, |d| {
                                d.border_color(accent_color())
                            }))
                            .when(table == Table::Form, |d| {
                                d.child(
                                    crate::project_settings::button(
                                        ("api-row-kind", row_id),
                                        if file { "File" } else { "Text" },
                                    )
                                    .h(px(26.))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.row_edit(
                                                id,
                                                table,
                                                ix,
                                                |data, ix| data[ix].file = !data[ix].file,
                                                cx,
                                            )
                                        },
                                    )),
                                )
                            })
                            .when(!fixed, |d| {
                                d.child(toggle(("api-row-on", row_id), !off).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.row_edit(
                                            id,
                                            table,
                                            ix,
                                            |data, ix| data[ix].disabled = !data[ix].disabled,
                                            cx,
                                        )
                                    },
                                )))
                                .child(
                                    small_button(("api-row-delete", row_id), "Remove", "trash")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.row_edit(
                                                id,
                                                table,
                                                ix,
                                                |data, ix| {
                                                    data.remove(ix);
                                                },
                                                cx,
                                            )
                                        })),
                                )
                            })
                    }),
            )
            .when(!fixed, |d| {
                d.child(
                    div()
                        .id(("api-row-add", base))
                        .role(Role::Button)
                        .aria_label(label)
                        .h(px(30.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(6.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_size(px(12.5))
                        .text_color(content(0.5))
                        .hover(|s| s.bg(content(0.06)).text_color(content(1.)))
                        .child(icon("plus", 12.))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| this.add_row(id, table, cx))),
                )
            })
            .into_any_element()
    }

    fn body_editor(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab(id) else {
            return div().into_any_element();
        };
        let mode = tab.body_mode.clone();
        let language = tab.raw_language;
        let body = tab.body.clone();
        let modes: [(BodyMode, &'static str); 4] = [
            (BodyMode::None, "none"),
            (BodyMode::Raw, "raw"),
            (BodyMode::UrlEncoded, "x-www-form-urlencoded"),
            (BodyMode::Form, "form-data"),
        ];
        let strip = div()
            .flex_shrink_0()
            .px(px(10.))
            .pb(px(6.))
            .flex()
            .items_center()
            .gap(px(2.))
            .children(modes.into_iter().enumerate().map(|(ix, (m, label))| {
                let on = mode == m;
                strip_label(
                    ("api-body-mode", id as usize * 8 + ix),
                    label.to_owned(),
                    on,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_body_mode(id, m.clone(), cx)))
            }))
            .when(mode == BodyMode::Raw, |d| {
                d.child(
                    crate::project_settings::button(
                        ("api-body-language", id as usize),
                        language.id().to_uppercase(),
                    )
                    .h(px(26.))
                    .child(icon("chevron-down", 11.).text_color(content(0.4)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            let items: Vec<MenuItem> = [
                                RawLanguage::Json,
                                RawLanguage::Text,
                                RawLanguage::Xml,
                                RawLanguage::Html,
                                RawLanguage::JavaScript,
                            ]
                            .iter()
                            .map(|l| MenuAction::new(l.id(), l.id().to_uppercase()).into())
                            .collect();
                            this.open_menu(
                                MenuKind::BodyLanguage(id),
                                items,
                                "Body language",
                                event.position,
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }),
                    ),
                )
            });
        let content_area = match mode {
            BodyMode::None => div()
                .flex_1()
                .px(px(14.))
                .py(px(8.))
                .text_size(px(12.5))
                .text_color(content(0.45))
                .child("This request has no body.")
                .into_any_element(),
            BodyMode::Raw => div()
                .flex_1()
                .min_h_0()
                .mx(px(14.))
                .mb(px(8.))
                .rounded(px(6.))
                .border_1()
                .border_color(content(0.1))
                .overflow_hidden()
                .child(body)
                .into_any_element(),
            BodyMode::UrlEncoded => self.rows_editor(id, Table::UrlEncoded, cx),
            BodyMode::Form => self.rows_editor(id, Table::Form, cx),
            BodyMode::Unsupported(kind) => div()
                .flex_1()
                .px(px(14.))
                .py(px(8.))
                .text_size(px(12.5))
                .text_color(content(0.45))
                .child(format!(
                    "A {kind} body is kept in the file but cannot be edited here yet."
                ))
                .into_any_element(),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(strip)
            .child(content_area)
            .into_any_element()
    }

    fn auth_editor(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab(id) else {
            return div().into_any_element();
        };
        let auth = tab.draft.auth.clone();
        let inherited = self
            .collection(&tab.file)
            .map(|c| c.collection.inherited_auth(&tab.path))
            .unwrap_or(Auth::None);
        let inputs = (
            tab.auth.username.clone(),
            tab.auth.password.clone(),
            tab.auth.token.clone(),
            tab.auth.key.clone(),
            tab.auth.value.clone(),
        );
        let labelled = |label: &'static str, input: Entity<Input>| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .w(px(90.))
                        .flex_shrink_0()
                        .text_size(px(12.5))
                        .text_color(content(0.55))
                        .child(label),
                )
                .child(field_box(input).max_w(px(420.)))
        };
        let note = |text: String| {
            div()
                .text_size(px(12.5))
                .text_color(content(0.45))
                .child(text)
        };
        div()
            .id(("api-auth", id as usize))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(14.))
            .py(px(6.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .w(px(90.))
                            .text_size(px(12.5))
                            .text_color(content(0.55))
                            .child("Type"),
                    )
                    .child(
                        crate::project_settings::button(
                            ("api-auth-kind", id as usize),
                            auth.label().to_owned(),
                        )
                        .h(px(30.))
                        .child(icon("chevron-down", 11.).text_color(content(0.4)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                let items: Vec<MenuItem> = vec![
                                    MenuAction::new("inherit", "Inherit from parent").into(),
                                    MenuAction::new("none", "No auth").into(),
                                    MenuAction::new("basic", "Basic").into(),
                                    MenuAction::new("bearer", "Bearer token").into(),
                                    MenuAction::new("apikey", "API key").into(),
                                ];
                                this.open_menu(
                                    MenuKind::Auth(id),
                                    items,
                                    "Authentication",
                                    event.position,
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            }),
                        ),
                    ),
            )
            .child(match auth {
                Auth::Inherit => note(format!(
                    "Uses the auth of its folder or collection: {}.",
                    inherited.label().to_lowercase()
                ))
                .into_any_element(),
                Auth::None => note("This request sends no credentials.".into()).into_any_element(),
                Auth::Basic { .. } => div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(labelled("Username", inputs.0))
                    .child(labelled("Password", inputs.1))
                    .into_any_element(),
                Auth::Bearer { .. } => div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(labelled("Token", inputs.2))
                    .into_any_element(),
                Auth::ApiKey { in_header, .. } => div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(labelled("Key", inputs.3))
                    .child(labelled("Value", inputs.4))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .w(px(90.))
                                    .text_size(px(12.5))
                                    .text_color(content(0.55))
                                    .child("Add to"),
                            )
                            .child(
                                strip_label(
                                    ("api-key-header", id as usize),
                                    "Header".into(),
                                    in_header,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.set_api_key_placement(id, true, cx),
                                )),
                            )
                            .child(
                                strip_label(
                                    ("api-key-query", id as usize),
                                    "Query".into(),
                                    !in_header,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.set_api_key_placement(id, false, cx),
                                )),
                            ),
                    )
                    .into_any_element(),
                Auth::Unsupported(kind) => note(format!(
                    "{kind} auth is kept in the file but not applied when sending."
                ))
                .into_any_element(),
            })
            .into_any_element()
    }

    fn scripts_editor(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab(id) else {
            return div().into_any_element();
        };
        let kind = tab.script_kind;
        let (editor, event) = match kind {
            ScriptKind::PreRequest => (tab.pre_script.clone(), "prerequest"),
            ScriptKind::Tests => (tab.test_script.clone(), "test"),
        };
        let owners = self.script_owners(tab, event);
        let empty = tab.draft.pre_request.trim().is_empty() && tab.draft.tests.trim().is_empty();
        let kinds = [
            (
                ScriptKind::PreRequest,
                if tab.draft.pre_request.trim().is_empty() {
                    "Pre-request"
                } else {
                    "Pre-request •"
                },
            ),
            (
                ScriptKind::Tests,
                if tab.draft.tests.trim().is_empty() {
                    "Tests"
                } else {
                    "Tests •"
                },
            ),
        ];
        let when = match kind {
            ScriptKind::PreRequest => "Runs before the request is sent.",
            ScriptKind::Tests => "Runs after the response arrives.",
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .px(px(10.))
                    .pb(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .children(kinds.into_iter().enumerate().map(|(ix, (k, label))| {
                        strip_label(
                            ("api-script-kind", id as usize * 4 + ix),
                            label.to_owned(),
                            kind == k,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(tab) = this.tab_mut(id) {
                                tab.script_kind = k;
                            }
                            cx.notify();
                        }))
                    }))
                    .child(
                        div()
                            .px(px(8.))
                            .text_size(px(11.5))
                            .text_color(content(0.45))
                            .child(when),
                    ),
            )
            .when(!owners.is_empty(), |d| {
                d.child(
                    div()
                        .px(px(14.))
                        .pb(px(6.))
                        .text_size(px(11.5))
                        .text_color(content(0.45))
                        .child(format!(
                            "Runs first: the {} script of {}.",
                            if kind == ScriptKind::PreRequest { "pre-request" } else { "test" },
                            owners.join(", ")
                        )),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mx(px(14.))
                    .mb(px(6.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(content(0.1))
                    .overflow_hidden()
                    .child(editor),
            )
            .when(empty, |d| {
                d.child(
                    div()
                        .px(px(14.))
                        .pb(px(8.))
                        .text_size(px(11.5))
                        .text_color(content(0.4))
                        .child(
                            "Postman scripts work: pm.environment.set(\"token\", pm.response.json().token), \
                             pm.test(\"ok\", () => pm.response.to.have.status(200)).",
                        ),
                )
            })
            .into_any_element()
    }

    fn section_body(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab(id) else {
            return div().into_any_element();
        };
        match tab.section {
            Section::Path => self.rows_editor(id, Table::Path, cx),
            Section::Query => self.rows_editor(id, Table::Query, cx),
            Section::Headers => self.rows_editor(id, Table::Headers, cx),
            Section::Body => self.body_editor(id, cx),
            Section::Auth => self.auth_editor(id, cx),
            Section::Scripts => self.scripts_editor(id, cx),
            Section::Notes => div()
                .flex_1()
                .px(px(14.))
                .py(px(6.))
                .child(field_box(tab.notes.clone()))
                .into_any_element(),
        }
    }

    // ---- the response ----

    fn response_panel(&mut self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tab(id) else {
            return div().into_any_element();
        };
        let height = self.response_height;
        let shell = |child: AnyElement| {
            div()
                .relative()
                .h(px(height))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(content(0.08))
                .child(child)
        };
        let splitter = div()
            .id("api-split")
            .role(Role::Splitter)
            .aria_label("Resize response")
            .absolute()
            .left_0()
            .right_0()
            .top(px(-3.))
            .h(px(6.))
            .cursor(CursorStyle::ResizeUpDown)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.resize = Some(Resize::Response(
                        f32::from(event.position.y),
                        this.response_height,
                    ));
                    cx.stop_propagation();
                }),
            );
        let message = |text: String, hex: Option<u32>| {
            div()
                .flex_1()
                .px(px(16.))
                .py(px(12.))
                .text_size(px(12.5))
                .text_color(hex.map_or(content(0.45), |h| rgb(h).into()))
                .child(text)
                .into_any_element()
        };
        match &tab.exchange {
            Exchange::Idle => shell(message(
                "Send the request to see its response.".into(),
                None,
            ))
            .child(splitter)
            .into_any_element(),
            Exchange::Sending => shell(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .text_size(px(12.5))
                    .text_color(content(0.5))
                    .child(crate::right_panel::spinner("api-sending", "loader", 14.))
                    .child("Sending…")
                    .into_any_element(),
            )
            .child(splitter)
            .into_any_element(),
            Exchange::Failed(error) => {
                let logs = tab.logs.clone();
                shell(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .px(px(16.))
                                .py(px(12.))
                                .text_size(px(12.5))
                                .text_color(rgb(0xf87171))
                                .child(error.clone()),
                        )
                        .when(!logs.is_empty(), |d| d.child(console_view(id, &logs)))
                        .into_any_element(),
                )
                .child(splitter)
                .into_any_element()
            }
            Exchange::Done(done) => {
                let current = tab.response_tab;
                let status = format!("{} {}", done.status, done.reason).trim().to_owned();
                let hex = status_color(done.status);
                let meta = format!(
                    "{} · {}{}",
                    human_time(done.elapsed),
                    human_size(done.size),
                    if done.truncated { " (cut)" } else { "" }
                );
                let headers = done.headers.clone();
                let viewer = tab.viewer.clone();
                let (tests, logs) = (tab.tests.clone(), tab.logs.clone());
                let passed = tests.iter().filter(|t| t.passed).count();
                let mut tabs: Vec<(ResponseTab, String)> = vec![
                    (ResponseTab::Pretty, "Pretty".into()),
                    (ResponseTab::Raw, "Raw".into()),
                    (ResponseTab::Headers, "Headers".into()),
                ];
                if !tests.is_empty() {
                    tabs.push((
                        ResponseTab::Tests,
                        format!(
                            "Tests {passed}/{}{}",
                            tests.len(),
                            if passed < tests.len() { " !" } else { "" }
                        ),
                    ));
                }
                if !logs.is_empty() {
                    tabs.push((ResponseTab::Console, format!("Console {}", logs.len())));
                }
                let body = if current == ResponseTab::Tests {
                    tests_view(id, &tests)
                } else if current == ResponseTab::Console {
                    console_view(id, &logs)
                } else if current == ResponseTab::Headers {
                    div()
                        .id(("api-response-headers", id as usize))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(px(16.))
                        .py(px(6.))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .children(headers.into_iter().map(|(name, value)| {
                            div()
                                .flex()
                                .gap(px(10.))
                                .text_size(px(12.5))
                                .child(
                                    div()
                                        .w(px(200.))
                                        .flex_shrink_0()
                                        .truncate()
                                        .text_color(content(0.55))
                                        .child(name),
                                )
                                .child(div().min_w_0().flex_1().child(value))
                        }))
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .min_h_0()
                        .mx(px(8.))
                        .mb(px(6.))
                        .overflow_hidden()
                        .child(viewer)
                        .into_any_element()
                };
                shell(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(36.))
                                .flex_shrink_0()
                                .px(px(10.))
                                .flex()
                                .items_center()
                                .gap(px(2.))
                                .children(tabs.into_iter().enumerate().map(
                                    |(ix, (kind, label))| {
                                        strip_label(
                                            ("api-response-tab", id as usize * 8 + ix),
                                            label,
                                            current == kind,
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.set_response_tab(id, kind, cx)
                                            }),
                                        )
                                    },
                                ))
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .px(px(8.))
                                        .py(px(2.))
                                        .rounded(px(4.))
                                        .text_size(px(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(hex))
                                        .bg(gpui::Hsla::from(rgb(hex)).opacity(0.15))
                                        .child(status),
                                )
                                .child(
                                    div()
                                        .px(px(8.))
                                        .text_size(px(12.))
                                        .text_color(content(0.5))
                                        .child(meta),
                                )
                                .child(
                                    small_button(
                                        ("api-copy-response", id as usize),
                                        "Copy response",
                                        "response-copy",
                                    )
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.copy_response(id, cx)
                                        }),
                                    ),
                                ),
                        )
                        .child(body)
                        .into_any_element(),
                )
                .child(splitter)
                .into_any_element()
            }
        }
    }

    fn main_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(id) = self.active.filter(|id| self.tab(*id).is_some()) else {
            return div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .child(self.tab_strip(cx))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(10.))
                        .text_color(content(0.5))
                        .child(icon("api", 28.).text_color(content(0.3)))
                        .child(
                            div()
                                .text_size(px(13.))
                                .child(if self.collections.is_empty() {
                                    "Create a collection or import one from Postman"
                                } else {
                                    "Pick a request from the tree"
                                }),
                        ),
                )
                .into_any_element();
        };
        let tabs = self.tab_strip(cx);
        let bar = self.request_bar(id, cx);
        let sections = self.section_strip(id, cx);
        let body = self.section_body(id, cx);
        let response = self.response_panel(id, cx);
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(tabs)
            .child(bar)
            .child(sections)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .child(response)
            .into_any_element()
    }

    // ---- environments ----

    fn env_editor_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editor = self.env_editor.as_ref()?;
        let is_environment = matches!(editor.target, EnvTarget::Environment(_));
        let in_use = match &editor.target {
            EnvTarget::Environment(file) => self.env_in_use(file),
            EnvTarget::Variables(_) => false,
        };
        let target = editor.target.clone();
        let confirm_delete = editor.confirm_delete;
        let list_item =
            |id: (&'static str, usize), label: String, detail: Option<String>, on: bool| {
                div()
                    .id(id)
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .h(px(28.))
                    .px(px(8.))
                    .rounded(px(5.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .text_color(content(if on { 1. } else { 0.7 }))
                    .when(on, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.07)))
                    .child(div().min_w_0().flex_1().truncate().child(label))
                    .when_some(detail, |d, text| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(10.5))
                                .text_color(content(0.4))
                                .child(text),
                        )
                    })
            };
        let environments: Vec<AnyElement> = self
            .environments
            .iter()
            .enumerate()
            .map(|(ix, env)| {
                let file = env.file.clone();
                let on = target == EnvTarget::Environment(file.clone());
                let used = self.env_in_use(&file);
                list_item(
                    ("api-env-item", ix),
                    env.environment.name(),
                    used.then(|| "in use".to_owned()),
                    on,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.switch_env_target(EnvTarget::Environment(file.clone()), cx)
                }))
                .into_any_element()
            })
            .collect();
        let collections: Vec<AnyElement> = self
            .collections
            .iter()
            .enumerate()
            .map(|(ix, loaded)| {
                let file = loaded.file.clone();
                let on = target == EnvTarget::Variables(file.clone());
                list_item(
                    ("api-env-collection", ix),
                    loaded.collection.name(),
                    None,
                    on,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.switch_env_target(EnvTarget::Variables(file.clone()), cx)
                }))
                .into_any_element()
            })
            .collect();
        let rows: Vec<(Entity<Input>, Entity<Input>, bool, bool)> = editor
            .rows
            .iter()
            .map(|r| (r.key.clone(), r.value.clone(), r.enabled, r.secret))
            .collect();
        let section = |label: &'static str| {
            div()
                .px(px(8.))
                .pt(px(8.))
                .pb(px(2.))
                .text_size(px(10.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(content(0.4))
                .child(label)
        };
        let header = if is_environment {
            let file = match &target {
                EnvTarget::Environment(file) => file.clone(),
                EnvTarget::Variables(file) => file.clone(),
            };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(field_box(editor.name.clone()).min_h(px(32.)))
                .child(if in_use {
                    div()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(5.))
                        .text_size(px(11.5))
                        .text_color(rgb(0x4ade80))
                        .bg(gpui::Hsla::from(rgb(0x4ade80)).opacity(0.15))
                        .child("In use")
                        .into_any_element()
                } else {
                    crate::project_settings::button("api-env-use", "Use")
                        .h(px(30.))
                        .on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.use_environment(file.clone(), cx)
                            }),
                        )
                        .into_any_element()
                })
                .child(if confirm_delete {
                    modal::danger_button("api-env-delete-confirm", "Delete?")
                        .h(px(30.))
                        .on_click(cx.listener(|this, _, _, cx| this.delete_environment(cx)))
                        .into_any_element()
                } else {
                    small_button("api-env-delete", "Delete environment", "trash")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(editor) = &mut this.env_editor {
                                editor.confirm_delete = true;
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .into_any_element()
        } else {
            div()
                .text_size(px(12.5))
                .text_color(content(0.5))
                .child("Variables for every request in this collection. An environment's value wins over these.")
                .into_any_element()
        };
        let card = modal::card().w(px(800.)).child(
            modal::card_body()
                .gap(px(12.))
                .child(modal::title("Environments and variables"))
                .child(
                    div()
                        .flex()
                        .gap(px(14.))
                        .h(px(380.))
                        .child(
                            div()
                                .id("api-env-list")
                                .w(px(210.))
                                .flex_shrink_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(section("ENVIRONMENTS"))
                                .children(environments)
                                .child(
                                    div()
                                        .id("api-env-new")
                                        .role(Role::Button)
                                        .aria_label("New environment")
                                        .h(px(28.))
                                        .px(px(8.))
                                        .rounded(px(5.))
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .cursor_pointer()
                                        .text_size(px(12.5))
                                        .text_color(content(0.5))
                                        .hover(|s| s.bg(content(0.07)).text_color(content(1.)))
                                        .child(icon("plus", 12.))
                                        .child("New environment")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.new_environment(cx)),
                                        ),
                                )
                                .child(section("COLLECTION VARIABLES"))
                                .children(collections),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(10.))
                                .child(header)
                                .child(
                                    div()
                                        .id("api-env-rows")
                                        .flex_1()
                                        .min_h_0()
                                        .overflow_y_scroll()
                                        .flex()
                                        .flex_col()
                                        .gap(px(6.))
                                        .when(rows.is_empty(), |d| {
                                            d.child(
                                                div()
                                                    .text_size(px(12.5))
                                                    .text_color(content(0.45))
                                                    .child("No variables yet."),
                                            )
                                        })
                                        .children(rows.into_iter().enumerate().map(
                                            |(ix, (key, value, enabled, secret))| {
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(8.))
                                                    .when(!enabled, |d| d.opacity(0.55))
                                                    .child(field_box(key))
                                                    .child(field_box(value))
                                                    .when(is_environment, |d| {
                                                        d.child(
                                                            small_button(
                                                                ("api-env-secret", ix),
                                                                "Hide the value (secret)",
                                                                "square-lock",
                                                            )
                                                            .when(secret, |d| {
                                                                d.bg(content(0.12))
                                                                    .text_color(content(1.))
                                                            })
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.toggle_env_secret(ix, cx)
                                                                },
                                                            )),
                                                        )
                                                    })
                                                    .child(
                                                        toggle(("api-env-on", ix), enabled)
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.env_row_edit(
                                                                        ix,
                                                                        |rows, ix| {
                                                                            rows[ix].enabled =
                                                                                !rows[ix].enabled
                                                                        },
                                                                        cx,
                                                                    )
                                                                },
                                                            )),
                                                    )
                                                    .child(
                                                        small_button(
                                                            ("api-env-row-delete", ix),
                                                            "Remove",
                                                            "trash",
                                                        )
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.env_row_edit(
                                                                    ix,
                                                                    |rows, ix| {
                                                                        rows.remove(ix);
                                                                    },
                                                                    cx,
                                                                )
                                                            },
                                                        )),
                                                    )
                                            },
                                        ))
                                        .child(
                                            div()
                                                .id("api-env-add-row")
                                                .role(Role::Button)
                                                .aria_label("Add variable")
                                                .h(px(30.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .gap(px(6.))
                                                .rounded(px(6.))
                                                .cursor_pointer()
                                                .text_size(px(12.5))
                                                .text_color(content(0.5))
                                                .hover(|s| {
                                                    s.bg(content(0.06)).text_color(content(1.))
                                                })
                                                .child(icon("plus", 12.))
                                                .child("Add variable")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.add_env_row(cx)
                                                })),
                                        ),
                                ),
                        ),
                )
                .child(div().text_size(px(11.5)).text_color(content(0.4)).child(
                    "Values are saved in the file as plain text, secret or not. \
                             Keep environment files out of git if they hold credentials.",
                ))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            crate::project_settings::button("api-env-cancel", "Cancel").on_click(
                                cx.listener(|this, _, _, cx| this.close_env_editor(false, cx)),
                            ),
                        )
                        .child(modal::primary_button("api-env-done", "Done").on_click(
                            cx.listener(|this, _, _, cx| this.close_env_editor(true, cx)),
                        )),
                ),
        );
        Some(
            modal::overlay("api-env", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if this.env_editor.as_ref().is_some_and(|e| !e.dirty) {
                            this.env_editor = None;
                            cx.notify();
                        }
                    });
                }
            })
            .into_any_element(),
        )
    }

    // ---- dialogs ----

    fn dialog_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let kind = self.dialog.as_ref()?;
        let cancel = |id: &'static str| {
            crate::project_settings::button(id, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                this.dialog = None;
                cx.notify();
            }))
        };
        let dismiss = {
            let weak = cx.weak_entity();
            move |_: &mut Window, cx: &mut gpui::App| {
                let _ = weak.update(cx, |this, cx| {
                    this.dialog = None;
                    cx.notify();
                });
            }
        };
        let card = match kind {
            DialogKind::Rename(_) | DialogKind::NewCollection => {
                let (title, action) = match kind {
                    DialogKind::NewCollection => ("New collection", "Create"),
                    _ => ("Rename", "Rename"),
                };
                modal::card().child(
                    modal::card_body()
                        .gap(px(10.))
                        .child(modal::title(title))
                        .child(
                            div()
                                .rounded(px(6.))
                                .border_1()
                                .border_color(content(0.12))
                                .bg(content(0.04))
                                .px_2()
                                .py(px(6.))
                                .child(self.dialog_input.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(cancel("api-dialog-cancel"))
                                .child(modal::primary_button("api-dialog-ok", action).on_click(
                                    cx.listener(|this, _, _, cx| this.confirm_dialog(cx)),
                                )),
                        ),
                )
            }
            DialogKind::Delete(target) => {
                let name = self
                    .collection(&target.file)
                    .and_then(|c| c.collection.item_name(&target.path))
                    .unwrap_or_default();
                let detail = if target.path.is_empty() {
                    "The collection's file is deleted from the project."
                } else {
                    "Anything inside it goes too. This is saved right away."
                };
                modal::card().child(
                    modal::card_body()
                        .gap(px(10.))
                        .child(modal::title(format!("Delete “{name}”?")))
                        .child(modal::body(detail))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(cancel("api-delete-cancel"))
                                .child(
                                    modal::danger_button("api-delete-confirm", "Delete").on_click(
                                        cx.listener(|this, _, _, cx| this.confirm_dialog(cx)),
                                    ),
                                ),
                        ),
                )
            }
            DialogKind::CloseDirty(id) => {
                let id = *id;
                let name = self
                    .tab(id)
                    .map(|t| t.draft.name.clone())
                    .unwrap_or_default();
                modal::card().child(
                    modal::card_body()
                        .gap(px(10.))
                        .child(modal::title(format!("Save changes to “{name}”?")))
                        .child(modal::body(
                            "Your edits are lost if you close without saving.",
                        ))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(cancel("api-close-cancel"))
                                .child(
                                    crate::project_settings::button("api-close-discard", "Discard")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.dialog = None;
                                            this.discard_tab(id, window, cx);
                                        })),
                                )
                                .child(modal::primary_button("api-close-save", "Save").on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        this.dialog = None;
                                        if this.save(id, cx) {
                                            this.discard_tab(id, window, cx);
                                        }
                                    }),
                                )),
                        ),
                )
            }
            DialogKind::CloseMany(ids) => {
                let ids = ids.clone();
                let total = ids.len();
                let changed = ids
                    .iter()
                    .filter(|id| self.tab(**id).is_some_and(RequestTab::dirty))
                    .count();
                let (discard_ids, save_ids) = (ids.clone(), ids);
                modal::card().child(
                    modal::card_body()
                        .gap(px(10.))
                        .child(modal::title(format!(
                            "Close {total} {}?",
                            if total == 1 { "request" } else { "requests" }
                        )))
                        .child(modal::body(format!(
                            "{changed} {} unsaved changes.",
                            if changed == 1 { "has" } else { "have" }
                        )))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(cancel("api-many-cancel"))
                                .child(
                                    crate::project_settings::button("api-many-discard", "Discard")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.dialog = None;
                                            this.discard_many(discard_ids.clone(), window, cx);
                                        })),
                                )
                                .child(
                                    modal::primary_button("api-many-save", "Save all").on_click(
                                        cx.listener(move |this, _, window, cx| {
                                            this.dialog = None;
                                            if this.save_all(cx) {
                                                this.discard_many(save_ids.clone(), window, cx);
                                            }
                                        }),
                                    ),
                                ),
                        ),
                )
            }
        };
        Some(modal::overlay("api-dialog", card, dismiss).into_any_element())
    }
}

impl Render for ApiView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tree = self.tree_panel(cx);
        let main = self.main_panel(cx);
        div()
            .id("api-view")
            .role(Role::TabPanel)
            .aria_label("API")
            .relative()
            .size_full()
            .flex()
            .track_focus(&self.focus)
            .key_context("Api")
            .text_color(content(1.))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let modifiers = event.keystroke.modifiers;
                let command = modifiers.platform || modifiers.control;
                let Some(id) = this.active else {
                    if event.keystroke.key == "escape" && this.env_editor.is_none() {
                        this.menu = None;
                        this.dialog = None;
                        cx.notify();
                    }
                    return;
                };
                let key = event.keystroke.key.as_str();
                match key {
                    "s" if command && modifiers.alt => {
                        this.save_all(cx);
                    }
                    "s" if command => {
                        this.save(id, cx);
                    }
                    "enter" if command => this.send(id, cx),
                    "w" if command && modifiers.shift => this.close_all(window, cx),
                    "w" if command => this.close_tab(id, window, cx),
                    "t" if command && modifiers.shift => this.reopen_closed(window, cx),
                    "l" if command => this.focus_url(window, cx),
                    "]" if command && modifiers.shift => this.step_tab(1, window, cx),
                    "[" if command && modifiers.shift => this.step_tab(-1, window, cx),
                    digit
                        if command && digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() =>
                    {
                        this.go_to_tab((digit.as_bytes()[0] - b'0') as usize, window, cx)
                    }
                    "escape" if this.menu.is_none() && this.env_editor.is_some() => {
                        if this.env_editor.as_ref().is_some_and(|e| !e.dirty) {
                            this.env_editor = None;
                            cx.notify();
                        }
                    }
                    "escape" if this.menu.is_some() || this.dialog.is_some() => {
                        this.menu = None;
                        this.dialog = None;
                        cx.notify();
                    }
                    "escape" => this.cancel(id, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                    match this.resize {
                        Some(Resize::Tree(start, width)) => {
                            let max =
                                (f32::from(window.viewport_size().width) * 0.5).clamp(240., 700.);
                            this.tree_width =
                                (width + f32::from(event.position.x) - start).clamp(200., max);
                        }
                        Some(Resize::Response(start, height)) => {
                            let max = (f32::from(window.viewport_size().height) - 320.).max(140.);
                            this.response_height =
                                (height + start - f32::from(event.position.y)).clamp(100., max);
                        }
                        None => return,
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.resize.take().is_some() {
                        this.save_state();
                        cx.notify();
                    }
                }),
            )
            .child(tree)
            .child(main)
            .when_some(self.notice.clone(), |d, text| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(14.))
                        .right(px(14.))
                        .max_w(px(420.))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .bg(content(0.12))
                        .text_size(px(12.))
                        .child(text),
                )
            })
            .children(
                self.menu
                    .as_ref()
                    .map(|open| menu_popover(&open.menu, Anchor::Point(open.anchor), cx).gap(0.)),
            )
            .children(self.dialog_overlay(cx))
            .children(self.env_editor_overlay(cx))
    }
}
