//! The data sources from the code editor: sending the selection to a
//! source's console, and running or explaining the SQL at the caret.
use super::{ExecuteSql, ExplainSql, FileEditor, FileEvent, selection::db_dot};
use crate::popover::{MenuAction, MenuItem};
use gpui::{App, Context, Window};

impl FileEditor {
    /// The code menu's data source rows.
    pub(super) fn sql_menu_items(
        &self,
        sources: &[crate::database::Target],
        cx: &App,
    ) -> Vec<MenuItem> {
        let selected = !self.input.read(cx).selection_range().is_empty();
        if !selected || sources.is_empty() {
            return Vec::new();
        }
        vec![
            MenuAction::new("database", "Send to Database")
                .icon("database")
                .submenu(
                    sources
                        .iter()
                        .enumerate()
                        .map(|(ix, source)| {
                            // A source that accepts writes says so.
                            let row = MenuAction::new(format!("db:{ix}"), source.name.clone())
                                .icon("dot")
                                .icon_color(db_dot(source.color));
                            if source.read_only {
                                row
                            } else {
                                row.value("writable")
                            }
                        })
                        .collect(),
                )
                .into(),
        ]
    }
    /// Takes a picked row of `sql_menu_items`; `false` when it is not one.
    pub(super) fn sql_menu_pick(
        &mut self,
        id: &str,
        target: &super::selection::Target,
        sources: &[crate::database::Target],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ix) = id
            .strip_prefix("db:")
            .and_then(|ix| ix.parse::<usize>().ok())
        else {
            return false;
        };
        if let (Some(source), Some(text)) = (sources.get(ix), self.selected_text(&target.range, cx))
        {
            cx.emit(FileEvent::SendToDatabase {
                source: source.id.clone(),
                text,
                first_line: target.lines.0,
                last_line: target.lines.1,
            });
        }
        true
    }
    pub(super) fn execute_sql(&mut self, _: &ExecuteSql, _: &mut Window, cx: &mut Context<Self>) {
        cx.propagate();
    }
    pub(super) fn explain_sql(&mut self, _: &ExplainSql, _: &mut Window, cx: &mut Context<Self>) {
        cx.propagate();
    }
}
