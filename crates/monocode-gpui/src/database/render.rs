//! Drawing the Database view.

use super::*;

fn color_of(color: Option<DbColor>) -> Option<u32> {
    Some(match color? {
        DbColor::Red => 0xf87171,
        DbColor::Orange => 0xfb923c,
        DbColor::Yellow => 0xfacc15,
        DbColor::Green => 0x4ade80,
        DbColor::Blue => 0x60a5fa,
        DbColor::Purple => 0xc084fc,
        DbColor::Gray => 0x9ca3af,
    })
}

fn pill(label: &'static str, hex: u32) -> AnyElement {
    div()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(hex))
        .bg(gpui::Hsla::from(rgb(hex)).opacity(0.15))
        .child(label)
        .into_any_element()
}

impl DatabaseView {
    fn tree_row(&mut self, ix: usize, row: &TreeRow, cx: &mut Context<Self>) -> AnyElement {
        let (depth, chevron, glyph, label, detail, selected): (
            usize,
            Option<bool>,
            &'static str,
            String,
            Option<String>,
            bool,
        ) = match row {
            TreeRow::Source(id) => {
                let source = self.source(id);
                let node = self.nodes.get(id);
                let open = self.expanded.contains(&format!("s:{id}"));
                let live = node.is_some_and(|n| matches!(n.link, Link::Ready));
                (
                    0,
                    Some(open),
                    "database",
                    source.map(|s| s.name.clone()).unwrap_or_default(),
                    live.then(|| "connected".to_owned()),
                    self.selected.as_deref() == Some(id),
                )
            }
            TreeRow::Schema(source, schema) => (
                1,
                Some(self.expanded.contains(&format!("n:{source}/{schema}"))),
                "folder",
                schema.clone(),
                None,
                false,
            ),
            TreeRow::Table(source, schema, i) => {
                let table = self
                    .nodes
                    .get(source)
                    .and_then(|n| n.schemas.as_ref())
                    .and_then(|s| s.iter().find(|s| &s.name == schema))
                    .and_then(|s| s.tables.as_ref())
                    .and_then(|t| t.get(*i));
                let name = table.map(|t| t.info.name.clone()).unwrap_or_default();
                let kind = table.map(|t| t.info.kind);
                (
                    2,
                    Some(
                        self.expanded
                            .contains(&format!("t:{source}/{schema}/{name}")),
                    ),
                    match kind {
                        Some(TableKind::View | TableKind::MaterializedView) => "eye",
                        _ => "table",
                    },
                    name,
                    table
                        .and_then(|t| t.info.estimated_rows)
                        .filter(|n| *n > 0)
                        .map(|n| format!("~{n}")),
                    false,
                )
            }
            TreeRow::Column(depth, column) => (
                *depth,
                None,
                if column.primary_key { "key" } else { "dot" },
                column.name.clone(),
                Some(column.type_name.clone()),
                false,
            ),
            TreeRow::Note(depth, text) => (*depth, None, "", text.clone(), None, false),
        };
        let is_note = matches!(row, TreeRow::Note(..));
        let tint = match row {
            TreeRow::Source(id) => color_of(self.source(id).and_then(|s| s.color)),
            _ => None,
        };
        let row = row.clone();
        div()
            .id(("database-tree-row", ix))
            .role(Role::TreeItem)
            .aria_label(label.clone())
            .h(px(TREE_ROW))
            .pl(px(8. + depth as f32 * 14.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(5.))
            .text_size(px(12.5))
            .text_color(content(if is_note { 0.4 } else { 0.85 }))
            .when(!is_note, |d| {
                d.cursor_pointer().hover(|s| s.bg(content(0.07)))
            })
            .when(selected, |d| d.bg(content(0.1)))
            .child(
                div()
                    .w(px(12.))
                    .flex_shrink_0()
                    .when_some(chevron, |d, open| {
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
                    }),
            )
            .when(!glyph.is_empty(), |d| {
                d.child(
                    icon(glyph, 14.)
                        .when_some(tint, |i, hex| i.text_color(rgb(hex)))
                        .when(tint.is_none(), |i| i.text_color(content(0.5))),
                )
            })
            .child(div().min_w_0().flex_1().truncate().child(label))
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
            .when(!is_note, |d| {
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                        this.click_row(&row, event.click_count, window, cx);
                    }),
                )
            })
            .into_any_element()
    }

    fn tree_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.rows();
        let selected = self.selected.clone();
        let entity = cx.entity();
        let header_button =
            |id: &'static str, label: &'static str, glyph: &'static str, on: bool| {
                icon_button(id, label, glyph)
                    .size(px(24.))
                    .opacity(if on { 1. } else { 0.35 })
            };
        div()
            .id("database-tree")
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
                            .child("DATA SOURCES"),
                    )
                    .child(
                        header_button("database-add", "New data source", "plus", true).on_click(
                            cx.listener(|this, _, window, cx| this.open_dialog(None, window, cx)),
                        ),
                    )
                    .child(
                        header_button(
                            "database-edit",
                            "Edit data source",
                            "pencil-edit",
                            selected.is_some(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(id) = this.selected.clone() {
                                this.open_dialog(Some(id), window, cx);
                            }
                        })),
                    )
                    .child(
                        header_button(
                            "database-refresh",
                            "Reconnect and refresh",
                            "refresh-cw",
                            selected.is_some(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(id) = this.selected.clone() {
                                this.expanded.insert(format!("s:{id}"));
                                this.refresh_source(&id, cx);
                            }
                        })),
                    )
                    .child(
                        header_button(
                            "database-remove",
                            "Remove data source",
                            "trash",
                            selected.is_some(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_remove = this.selected.clone();
                            cx.notify();
                        })),
                    ),
            )
            .child(if self.sources.is_empty() {
                div()
                    .flex_1()
                    .px(px(14.))
                    .py(px(10.))
                    .text_size(px(12.))
                    .text_color(content(0.45))
                    .child("No data sources yet. Add one with +.")
                    .into_any_element()
            } else {
                uniform_list("database-tree-rows", rows.len(), move |range, _, cx| {
                    entity.update(cx, |this, cx| {
                        let rows = this.rows();
                        range
                            .filter_map(|ix| rows.get(ix).cloned().map(|row| (ix, row)))
                            .map(|(ix, row)| this.tree_row(ix, &row, cx))
                            .collect::<Vec<_>>()
                    })
                })
                .flex_1()
                .min_h_0()
                .into_any_element()
            })
            .child(
                div()
                    .id("database-tree-resize")
                    .role(Role::Splitter)
                    .aria_label("Resize data sources")
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

    fn tabs(&mut self, source: &str, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active.get(source).copied();
        let tabs: Vec<(u32, String)> = self
            .consoles
            .iter()
            .filter(|c| c.source == source)
            .map(|c| (c.id, self.tab_title(c, cx)))
            .collect();
        div()
            .h(px(34.))
            .flex_shrink_0()
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(2.))
            .border_b_1()
            .border_color(content(0.07))
            .children(tabs.into_iter().map(|(id, title)| {
                let on = active == Some(id);
                let src = source.to_owned();
                let src_close = source.to_owned();
                div()
                    .id(("database-tab", id as usize))
                    .role(Role::Tab)
                    .aria_label(title.clone())
                    .aria_selected(on)
                    .group("database-tab")
                    .h(px(26.))
                    .pl(px(10.))
                    .pr(px(4.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(content(if on { 1. } else { 0.55 }))
                    .when(on, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.07)))
                    .child(title)
                    .child(
                        div()
                            .id(("database-tab-close", id as usize))
                            .role(Role::Button)
                            .aria_label("Close console")
                            .size(px(18.))
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(content(0.4))
                            .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                            .child(icon("close", 10.))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_console(&src_close, id, window, cx);
                                cx.stop_propagation();
                            })),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.activate(&src, id, window, cx)),
                    )
            }))
            .child(
                icon_button("database-new-console", "New console", "plus")
                    .size(px(24.))
                    .on_click(cx.listener(|this, _, window, cx| this.new_console(window, cx))),
            )
            .into_any_element()
    }

    fn toolbar(&mut self, source: &DataSource, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let running = self.consoles[ix].running;
        let (src, id) = (self.consoles[ix].source.clone(), self.consoles[ix].id);
        let (src_stop, src_run) = (src.clone(), src.clone());
        let limit = self.limit;
        div()
            .h(px(36.))
            .flex_shrink_0()
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(if running {
                icon_button("database-stop", "Stop (Esc)", "stop")
                    .size(px(26.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cancel(&src_stop, id);
                        cx.notify();
                    }))
                    .into_any_element()
            } else {
                icon_button("database-run", "Run (⌘↩)", "play")
                    .size(px(26.))
                    .on_click(cx.listener(move |this, _, _, cx| this.run(&src_run, id, false, cx)))
                    .into_any_element()
            })
            .child(
                crate::project_settings::button("database-run-all", "Run all")
                    .h(px(26.))
                    .on_click(cx.listener({
                        let src = src.clone();
                        move |this, _, _, cx| this.run(&src, id, true, cx)
                    })),
            )
            .when(running, |d| {
                d.child(crate::right_panel::spinner(
                    "database-running",
                    "loader",
                    14.,
                ))
            })
            .child(div().flex_1())
            .child(
                crate::project_settings::button("database-limit", format!("limit {limit}"))
                    .h(px(26.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let next = LIMITS
                            .iter()
                            .position(|l| *l == this.limit)
                            .map_or(0, |i| (i + 1) % LIMITS.len());
                        this.limit = LIMITS[next];
                        this.save_state();
                        cx.notify();
                    })),
            )
            .child(if source.read_only {
                pill("read-only", 0x4ade80)
            } else {
                pill("writes allowed", 0xfb923c)
            })
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(content(0.5))
                    .child(source.name.clone()),
            )
            .into_any_element()
    }

    fn status_line(&self, ix: usize) -> String {
        let console = &self.consoles[ix];
        if console.running {
            return "Running…".into();
        }
        let Some(result) = &console.result else {
            return String::new();
        };
        let shown = if result.columns.is_empty() {
            match result.affected {
                Some(n) => format!("{n} rows affected"),
                None => "Done".into(),
            }
        } else {
            format!(
                "{} row{}{}",
                result.rows.len(),
                if result.rows.len() == 1 { "" } else { "s" },
                if result.has_more {
                    " (more available — raise the limit)"
                } else {
                    ""
                }
            )
        };
        format!(
            "{shown} · {:.0} ms to first row, {:.0} ms total",
            result.execution_ms, result.elapsed_ms
        )
    }

    fn error_panel(error: &DbError, statement: &str) -> AnyElement {
        let location = error.position.map(|byte| {
            let before = &statement[..byte.min(statement.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("Line {line}, column {col}")
        });
        div()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(px(12.5))
            .child(
                div()
                    .text_color(rgb(0xf87171))
                    .font_weight(FontWeight::MEDIUM)
                    .child(if error.message.is_empty() {
                        "The statement did not finish".to_owned()
                    } else {
                        error.message.clone()
                    }),
            )
            .when_some(error.detail.clone(), |d, t| {
                d.child(div().text_color(content(0.6)).child(format!("Detail: {t}")))
            })
            .when_some(error.hint.clone(), |d, t| {
                d.child(div().text_color(content(0.6)).child(format!("Hint: {t}")))
            })
            .when_some(location, |d, t| {
                d.child(div().text_color(content(0.45)).child(t))
            })
            .when_some(error.code.clone(), |d, t| {
                d.child(
                    div()
                        .text_size(px(11.))
                        .text_color(content(0.35))
                        .child(format!("SQLSTATE {t}")),
                )
            })
            .into_any_element()
    }

    fn grid_row(&mut self, ix: usize, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(console) = self.consoles.get(ix) else {
            return div().into_any_element();
        };
        let Some(result) = console.result.clone() else {
            return div().into_any_element();
        };
        let selected = console.selected;
        let total: f32 = console.widths.iter().sum();
        let widths = console.widths.clone();
        let numeric = console.numeric.clone();
        let data = &result.rows[row];
        div()
            .w(px(total))
            .h(px(grid::ROW_HEIGHT))
            .flex()
            .when(row % 2 == 1, |d| d.bg(content(0.025)))
            .when(selected.is_some_and(|(r, _)| r == row), |d| {
                d.bg(content(0.06))
            })
            .children(widths.into_iter().enumerate().map(|(col, width)| {
                let value = data.get(col).and_then(|v| v.as_deref());
                let is_selected = selected == Some((row, col));
                div()
                    .id(("database-cell", row * 4096 + col))
                    .w(px(width))
                    .h_full()
                    .flex_shrink_0()
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .when(numeric.get(col).copied().unwrap_or(false), |d| {
                        d.justify_end()
                    })
                    .overflow_hidden()
                    .border_r_1()
                    .border_color(content(0.05))
                    .text_size(px(12.))
                    .when(is_selected, |d| {
                        d.bg(accent_color().opacity(0.25))
                            .border_1()
                            .border_color(accent_color().opacity(0.7))
                    })
                    .child(match value {
                        Some(text) => div()
                            .truncate()
                            .text_color(content(0.9))
                            .child(grid::display_cell(text))
                            .into_any_element(),
                        None => div()
                            .italic()
                            .text_color(content(0.3))
                            .child("NULL")
                            .into_any_element(),
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            if let Some(c) = this.consoles.get_mut(ix) {
                                c.selected = Some((row, col));
                            }
                            window.focus(&this.grid_focus, cx);
                            if event.click_count >= 2 {
                                this.open_preview(ix, row, col);
                            }
                            cx.notify();
                        }),
                    )
            }))
            .into_any_element()
    }

    fn results(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let status = self.status_line(ix);
        let console = &self.consoles[ix];
        let body: AnyElement = if let Some((error, statement)) = console.error.clone() {
            Self::error_panel(&error, &statement)
        } else if let Some(result) = console.result.clone() {
            if result.columns.is_empty() {
                div()
                    .p(px(12.))
                    .text_size(px(12.5))
                    .text_color(content(0.6))
                    .child(result.statement.lines().next().unwrap_or("").to_owned())
                    .into_any_element()
            } else {
                let widths = console.widths.clone();
                let total: f32 = widths.iter().sum();
                let rows = result.rows.len();
                let entity = cx.entity();
                div()
                    .id("database-grid")
                    .track_focus(&self.grid_focus)
                    .key_context("DatabaseGrid")
                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                        this.grid_key(event, cx)
                    }))
                    .flex_1()
                    .min_h_0()
                    .overflow_x_scroll()
                    .child(
                        div()
                            .w(px(total.max(1.)))
                            .min_w_full()
                            .h_full()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .w(px(total))
                                    .h(px(grid::HEADER_HEIGHT))
                                    .flex_shrink_0()
                                    .flex()
                                    .border_b_1()
                                    .border_color(content(0.1))
                                    .children(result.columns.iter().zip(widths).map(
                                        |(column, width)| {
                                            div()
                                                .w(px(width))
                                                .h_full()
                                                .flex_shrink_0()
                                                .px(px(8.))
                                                .flex()
                                                .items_center()
                                                .border_r_1()
                                                .border_color(content(0.07))
                                                .text_size(px(11.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(content(0.6))
                                                .child(div().truncate().child(column.name.clone()))
                                        },
                                    )),
                            )
                            .child(
                                uniform_list("database-grid-rows", rows, move |range, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        range
                                            .map(|row| this.grid_row(ix, row, cx))
                                            .collect::<Vec<_>>()
                                    })
                                })
                                .flex_1()
                                .min_h_0(),
                            ),
                    )
                    .into_any_element()
            }
        } else {
            div()
                .p(px(14.))
                .text_size(px(12.5))
                .text_color(content(0.4))
                .child("Run a statement with ⌘↩ — the result appears here.")
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(body)
            .child(
                div()
                    .h(px(26.))
                    .flex_shrink_0()
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(content(0.07))
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(status),
            )
            .into_any_element()
    }

    fn preview_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, text) = self.preview.clone()?;
        const CAP: usize = 100_000;
        let cut = text.chars().count() > CAP;
        let shown: String = text.chars().take(CAP).collect();
        let copy = text.clone();
        let card = modal::card().w(px(760.)).child(
            modal::card_body()
                .gap(px(10.))
                .child(modal::title(title))
                .child(
                    div()
                        .id("database-preview-text")
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .rounded(px(6.))
                        .bg(content(0.05))
                        .p(px(10.))
                        .font_family("monospace")
                        .text_size(px(12.))
                        .text_color(content(0.9))
                        .child(SharedString::from(shown)),
                )
                .when(cut, |d| {
                    d.child(
                        div().text_size(px(11.)).text_color(content(0.45)).child(
                            "Showing the first 100,000 characters. Copy takes the whole value.",
                        ),
                    )
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            crate::project_settings::button("database-preview-copy", "Copy")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                })),
                        )
                        .child(
                            modal::primary_button("database-preview-close", "Close").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.preview = None;
                                    cx.notify();
                                }),
                            ),
                        ),
                ),
        );
        Some(
            modal::overlay("database-preview", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.preview = None;
                        cx.notify();
                    });
                }
            })
            .into_any_element(),
        )
    }

    fn remove_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = self.confirm_remove.clone()?;
        let name = self.source(&id)?.name.clone();
        let card = modal::card().child(
            modal::card_body()
                .gap(px(10.))
                .child(modal::title(format!("Remove “{name}”?")))
                .child(modal::body(
                    "Its consoles and saved password are deleted. The database itself is untouched.",
                ))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            crate::project_settings::button("database-remove-cancel", "Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_remove = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            modal::danger_button("database-remove-confirm", "Remove").on_click(
                                cx.listener(move |this, _, _, cx| this.remove_source(&id, cx)),
                            ),
                        ),
                ),
        );
        Some(
            modal::overlay("database-remove", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.confirm_remove = None;
                        cx.notify();
                    });
                }
            })
            .into_any_element(),
        )
    }

    fn main_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(source) = self
            .selected
            .clone()
            .and_then(|id| self.source(&id).cloned())
        else {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .text_color(content(0.5))
                .child(icon("database", 28.).text_color(content(0.3)))
                .child(
                    div()
                        .text_size(px(13.))
                        .child("Add a PostgreSQL data source to start"),
                )
                .child(
                    modal::primary_button("database-empty-add", "New data source").on_click(
                        cx.listener(|this, _, window, cx| this.open_dialog(None, window, cx)),
                    ),
                )
                .into_any_element();
        };
        let Some(ix) = self.active_index() else {
            return div().flex_1().into_any_element();
        };
        let tabs = self.tabs(&source.id, cx);
        let toolbar = self.toolbar(&source, ix, cx);
        let editor = self.consoles[ix].editor.clone();
        let results = self.results(ix, cx);
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(tabs)
            .child(toolbar)
            .child(
                div()
                    .h(px(self.editor_height))
                    .flex_shrink_0()
                    .relative()
                    .child(div().size_full().overflow_hidden().child(editor))
                    .child(
                        div()
                            .id("database-split")
                            .role(Role::Splitter)
                            .aria_label("Resize editor")
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(-3.))
                            .h(px(6.))
                            .cursor(CursorStyle::ResizeUpDown)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                                    this.resize = Some(Resize::Editor(
                                        f32::from(event.position.y),
                                        this.editor_height,
                                    ));
                                    cx.stop_propagation();
                                }),
                            ),
                    ),
            )
            .child(div().h(px(1.)).flex_shrink_0().bg(content(0.08)))
            .child(results)
            .into_any_element()
    }
}

impl Render for DatabaseView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tree = self.tree_panel(cx);
        let main = self.main_panel(cx);
        div()
            .id("database-view")
            .role(Role::TabPanel)
            .aria_label("Database")
            .relative()
            .size_full()
            .flex()
            .track_focus(&self.focus)
            .key_context("Database")
            .text_color(content(1.))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let modifiers = event.keystroke.modifiers;
                let command = modifiers.platform || modifiers.control;
                match event.keystroke.key.as_str() {
                    "escape" => {
                        let Some(ix) = this.active_index() else {
                            return;
                        };
                        if !this.consoles[ix].running {
                            return;
                        }
                        let (source, id) = (this.consoles[ix].source.clone(), this.consoles[ix].id);
                        this.cancel(&source, id);
                    }
                    "t" if command => this.new_console(window, cx),
                    "w" if command => {
                        let Some(ix) = this.active_index() else {
                            return;
                        };
                        let (source, id) = (this.consoles[ix].source.clone(), this.consoles[ix].id);
                        this.close_console(&source, id, window, cx);
                    }
                    "enter" if command && modifiers.shift => {
                        let Some(ix) = this.active_index() else {
                            return;
                        };
                        let (source, id) = (this.consoles[ix].source.clone(), this.consoles[ix].id);
                        this.run(&source, id, true, cx);
                    }
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
                        Some(Resize::Editor(start, height)) => {
                            let max = (f32::from(window.viewport_size().height) - 260.).max(120.);
                            this.editor_height =
                                (height + f32::from(event.position.y) - start).clamp(80., max);
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
                        .bottom(px(34.))
                        .right(px(12.))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .bg(content(0.1))
                        .text_size(px(12.))
                        .child(text),
                )
            })
            .children(self.preview_overlay(cx))
            .children(self.remove_overlay(cx))
            .children(self.dialog.as_ref().map(|(dialog, _)| dialog.clone()))
    }
}
