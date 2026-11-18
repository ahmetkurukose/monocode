//! Steps 1 to 6: the language servers start, their diagnostics are drawn,
//! completion and signature help while typing, hover, go to definition and
//! back, find usages, the Outline tab and symbol search.
use super::*;
use crate::code_shell::{chips, panel_tabs};
use monocode_core::code::{lsp::ServerState, services::services};

/// The file the code insight steps work in.
pub(super) const ORDERS: &str = "shop/orders.py";

impl Driver {
    pub(super) async fn insight(&mut self, cx: &mut Cx) {
        self.servers_start(cx).await;
        self.diagnostics(cx).await;
        self.completion(cx).await;
        self.hover_and_definition(cx).await;
        self.usages(cx).await;
        self.outline_and_symbols(cx).await;
    }

    /// The project's servers as `name: state`.
    fn servers(&self) -> Vec<(String, ServerState)> {
        (services().lsp().statuses().into_iter())
            .filter(|status| status.root.starts_with(&self.root))
            .map(|status| (status.name, status.state))
            .collect()
    }

    async fn servers_start(&mut self, cx: &mut Cx) {
        let opened = self.open(cx, ORDERS).await;
        // What the chip said on the way, each state once.
        let mut chip: Vec<String> = Vec::new();
        let mut ready = false;
        for _ in 0..600 {
            let state = self
                .app(cx, |_, cx| {
                    chips::find("code-servers", cx).and_then(|chip| (chip.state)(cx))
                })
                .flatten()
                .map_or("hidden".to_owned(), |state| {
                    format!("{} ({:?})", state.text, state.tone)
                });
            if chip.last() != Some(&state) {
                chip.push(state);
            }
            let servers = self.servers();
            ready = servers.len() >= 2
                && (servers.iter()).all(|(_, state)| matches!(state, ServerState::Ready));
            if ready {
                break;
            }
            self.pause(cx, 100).await;
        }
        self.pause(cx, 300).await;
        let servers: Vec<String> = (self.servers().iter())
            .map(|(name, state)| format!("{name}: {state:?}"))
            .collect();
        let busy_first = chip.iter().any(|state| state.contains("Busy"));
        let quiet_last = self
            .app(cx, |_, cx| {
                chips::find("code-servers", cx).and_then(|chip| (chip.state)(cx))
            })
            .flatten()
            .is_some_and(|state| state.tone == chips::Tone::Quiet);
        let drawn = self.bounds(cx, "code-servers").is_some();
        self.shot(cx, "file-open-servers-ready").await;
        self.record(
            "1",
            "a Python file opens and its language servers go from starting to ready",
            opened && ready && busy_first && quiet_last && drawn,
            json!({
                "opened": opened, "servers": servers, "chip_states": chip,
                "chip_drawn": drawn,
            }),
        );
    }

    async fn diagnostics(&mut self, cx: &mut Cx) {
        let path = self.path(ORDERS);
        let listed = |cx: &mut App| -> Vec<(String, usize, String, String)> {
            let hub = code_editor::hub(cx);
            let hub = hub.read(cx);
            let Some(open) = hub.document(&path) else {
                return Vec::new();
            };
            (hub.diagnostics(&path, None).iter())
                .map(|found| {
                    (
                        found.source.to_string(),
                        code_editor::row_of(&open.text, found.range.start) + 1,
                        format!("{:?}", found.severity),
                        found.message.to_string(),
                    )
                })
                .collect()
        };
        let type_line = self.row(cx, "DEFAULT_QUANTITY").map(|row| row + 1);
        let import_line = self.row(cx, "import os").map(|row| row + 1);
        let on = |found: &[(String, usize, String, String)], source: &str, line: Option<usize>| {
            (found.iter()).any(|(from, at, _, _)| from == source && Some(*at) == line)
        };
        let both = self
            .until(cx, 30_000, |_, cx| {
                let found = listed(cx);
                on(&found, "basedpyright", type_line) && on(&found, "ruff", import_line)
            })
            .await;
        self.pause(cx, 500).await;
        let found = self.app(cx, |_, cx| listed(cx)).unwrap_or_default();
        let counts = self
            .input(cx, |input, _| input.diagnostic_counts())
            .map(|counts| (counts.errors, counts.warnings, counts.infos + counts.hints));
        let header = self.bounds(cx, "file-problems").is_some();
        self.shot(cx, "diagnostics").await;
        // The count in the header opens the file's problems.
        let listed_problems = self.click_on(cx, "file-problems").await;
        self.shot(cx, "diagnostics-problems-list").await;
        self.keys(cx, "escape").await;
        self.record(
            "2",
            "the type error and the unused import are marked on their lines and counted in the header",
            both && header && counts.is_some_and(|(errors, warnings, _)| errors + warnings >= 2),
            json!({
                "type_error_line": type_line, "unused_import_line": import_line,
                "diagnostics": found, "header_counts": counts, "header_count_drawn": header,
                "problems_list_clicked": listed_problems,
            }),
        );
    }

    async fn completion(&mut self, cx: &mut Cx) {
        let before = self.text(cx);
        // A click at the end of `text = ""`, then typing a member access.
        let mut detail = json!({});
        let mut pass = false;
        if let Some((_, at)) = self.glyph(cx, "text = \"\"") {
            self.click(cx, at).await;
            self.keys(cx, "end").await;
            self.type_text(cx, " + order.sc").await;
            let (listed, shown, narrow) =
                self.completion_popup(cx, "completion-popup-narrow").await;
            self.keys(cx, "enter").await;
            self.pause(cx, 300).await;
            let accepted = self.text(cx).contains("text = \"\" + order.scaled");
            self.type_text(cx, "(").await;
            let signature = |cx: &mut App| {
                let editor = context::focused_file(cx)?.editor.upgrade()?;
                let input = editor.read(cx).input.clone();
                let shown = input.read(cx).signature_shown()?;
                Some((shown.label.clone(), shown.active))
            };
            let helped = self.until(cx, 8000, |_, cx| signature(cx).is_some()).await;
            let signature = self.app(cx, |_, cx| signature(cx)).flatten();
            self.shot(cx, "signature-help").await;
            let line = (self.text(cx).lines())
                .find(|line| line.contains("order.scaled"))
                .map(str::to_owned);
            // The same list in a window with room: the sidebar put away.
            self.keys(cx, "escape escape").await;
            self.input(cx, |input, cx| input.set_text(before.clone(), cx));
            self.shell(cx, |shell, _, cx| shell.set_sidebar_visible(false, cx));
            self.pause(cx, 600).await;
            let mut wide = None;
            if let Some((_, at)) = self.glyph(cx, "text = \"\"") {
                self.click(cx, at).await;
                // On a line of its own, where the pane has room on the right.
                self.keys(cx, "end enter").await;
                self.type_text(cx, "order.sc").await;
                wide = Some(self.completion_popup(cx, "completion-popup-wide").await);
            }
            self.shell(cx, |shell, _, cx| shell.set_sidebar_visible(true, cx));
            self.pause(cx, 400).await;
            let wide_ok = wide
                .as_ref()
                .is_some_and(|(listed, _, placed)| *listed && placed["documentation"] == "right");
            pass = listed
                && narrow["clear"] == true
                && wide_ok
                && accepted
                && helped
                && signature
                    .as_ref()
                    .is_some_and(|(label, _)| label.contains("factor"));
            detail = json!({
                "typed": " + order.sc", "popup_has_scaled": listed, "popup": shown,
                "accepted_with_enter": accepted, "line_after": line,
                "narrow_pane": narrow, "wide_pane": wide.map(|(_, _, placed)| placed),
                "signature": signature,
            });
        }
        // The file as it was, so the later steps see the sample unchanged.
        self.keys(cx, "escape escape").await;
        self.input(cx, |input, cx| input.set_text(before.clone(), cx));
        self.keys(cx, "cmd-s").await;
        self.pause(cx, 500).await;
        self.record(
            "3",
            "typing a member access lists the real members, Enter accepts one, and ( shows the signature",
            pass,
            detail,
        );
    }

    /// Types nothing: waits for the member list that is being typed for, saves
    /// the picture, and says where the documentation box is against the list.
    async fn completion_popup(&mut self, cx: &mut Cx, shot: &str) -> (bool, Vec<String>, Value) {
        let labels = |input: &mut Input| input.completion_labels();
        let listed = self
            .until(cx, 12_000, |_, cx| {
                context::focused_file(cx)
                    .and_then(|file| file.editor.upgrade())
                    .is_some_and(|editor| {
                        let input = editor.read(cx).input.clone();
                        input.update(cx, |input, _| {
                            labels(input).iter().any(|label| label == "scaled")
                        })
                    })
            })
            .await;
        let shown = self.input(cx, |input, _| labels(input)).unwrap_or_default();
        self.pause(cx, 1200).await;
        self.shot(cx, shot).await;
        let list = self.bounds(cx, "editor-completions");
        let doc = self.bounds(cx, "editor-completion-documentation");
        let placed = match (list, doc) {
            (Some(list), Some(doc)) => {
                let apart = list.intersect(&doc).size.width.as_f32() <= 0.
                    || list.intersect(&doc).size.height.as_f32() <= 0.;
                let side = if doc.origin.x >= list.right() {
                    "right"
                } else if doc.right() <= list.origin.x {
                    "left"
                } else {
                    "above or below"
                };
                json!({
                    "documentation": side,
                    "clear": apart, "list": format!("{list:?}"), "doc": format!("{doc:?}"),
                })
            }
            (Some(_), None) => json!({ "documentation": "none", "clear": true }),
            _ => json!({ "documentation": "no list", "clear": false }),
        };
        (listed, shown, placed)
    }

    async fn hover_and_definition(&mut self, cx: &mut Cx) {
        let hovered = |cx: &mut App| {
            let editor = context::focused_file(cx)?.editor.upgrade()?;
            let input = editor.read(cx).input.clone();
            let (_, text) = input.read(cx).hover_text()?;
            Some(text.to_string())
        };
        // The mouse at rest over the parameter `order`.
        let mut hover = None;
        if let Some((_, at)) = self.glyph(cx, "order, 50)") {
            self.move_to(cx, at + point(px(4.), px(0.)), Modifiers::default())
                .await;
            self.until(cx, 8000, |_, cx| hovered(cx).is_some()).await;
            hover = self.app(cx, |_, cx| hovered(cx)).flatten();
            self.shot(cx, "hover").await;
        }
        let hover_ok = hover.as_ref().is_some_and(|text| text.contains("Order"));
        self.keys(cx, "escape").await;

        // Cmd held over a name defined in the other module, then a click.
        let held = Modifiers::secondary_key();
        let from = self.place(cx);
        let mut landed = None;
        let mut underlined = false;
        if let Some((_, at)) = self.glyph(cx, "apply_discount(order, 50)") {
            let at = at + point(px(20.), px(0.));
            self.move_to(cx, at, held).await;
            self.pause(cx, 1500).await;
            self.shot(cx, "cmd-hover-link").await;
            underlined = true;
            self.press(cx, at, MouseButton::Left, held).await;
            let target = self.path("shop/models.py").to_string_lossy().into_owned();
            self.until(cx, 8000, |_, cx| {
                context::focused_file(cx).is_some_and(|file| file.path == target)
            })
            .await;
            self.pause(cx, 500).await;
            landed = self.place(cx);
            self.shot(cx, "definition-in-other-file").await;
        }
        let definition_line = self.row(cx, "def apply_discount").map(|row| row + 1);
        let definition_ok = landed.as_ref().is_some_and(|(path, line, _)| {
            path.ends_with("shop/models.py") && Some(*line) == definition_line
        });
        // Back, from the keyboard.
        self.keys(cx, "ctrl--").await;
        let origin = self.path(ORDERS).to_string_lossy().into_owned();
        let returned = self
            .until(cx, 5000, |_, cx| {
                context::focused_file(cx).is_some_and(|file| file.path == origin)
            })
            .await;
        let back = self.place(cx);
        self.record(
            "4",
            "hover shows the type; Cmd-click opens the definition in the other module; Back returns",
            hover_ok && definition_ok && returned,
            json!({
                "hover": hover, "link_tried": underlined, "from": from,
                "landed": landed, "definition_line": definition_line, "back": back,
            }),
        );
    }

    async fn usages(&mut self, cx: &mut Cx) {
        let mut found = None;
        let mut went = None;
        let before = self.place(cx);
        if let Some((_, at)) = self.glyph(cx, "apply_discount(order, 80)") {
            self.click(cx, at + point(px(20.), px(0.))).await;
            self.keys(cx, "shift-f12").await;
            self.until(cx, 20_000, |_, cx| {
                crate::code_insight::probe::usages(cx).is_some()
            })
            .await;
            self.pause(cx, 600).await;
            found = self
                .app(cx, |_, cx| crate::code_insight::probe::usages(cx))
                .flatten();
            self.shot(cx, "usages-tab").await;
            // The tab has the keyboard: the next usage, then Enter.
            self.keys(cx, "down enter").await;
            self.pause(cx, 800).await;
            went = self.place(cx);
            self.shot(cx, "usage-opened").await;
        }
        let tab_drawn = self.bounds(cx, "code-usages").is_some();
        let listed = found.as_ref().is_some_and(|(symbol, total, lines)| {
            symbol == "apply_discount" && *total >= 7 && !lines.is_empty()
        });
        // Where it went is one of the listed places, and not where it was.
        let navigated = match (&found, &went) {
            (Some((_, _, lines)), Some((path, line, _))) => {
                let place = format!("{}:{line}:", file_name(Path::new(path)));
                went != before && lines.iter().any(|listed| listed.starts_with(&place))
            }
            _ => false,
        };
        self.record(
            "5",
            "find usages lists every use in the Usages tab and Enter on one opens it",
            listed && navigated && tab_drawn,
            json!({
                "usages": found, "before": before, "after_enter": went, "tab_drawn": tab_drawn,
            }),
        );
        self.open(cx, ORDERS).await;
    }

    async fn outline_and_symbols(&mut self, cx: &mut Cx) {
        let revealed = self
            .app(cx, |window, cx| {
                panel_tabs::reveal("code-outline", window, cx)
            })
            .unwrap_or(false);
        self.pause(cx, 600).await;
        let names: Vec<String> = self
            .app(cx, |_, cx| crate::code_insight::probe::outline(cx))
            .flatten()
            .unwrap_or_default()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let expected = ["make_order", "half_price", "clearance", "checkout", "label"];
        let outline_ok = (expected.iter()).all(|name| names.iter().any(|listed| listed == name));
        let outline_drawn = self.bounds(cx, "code-outline").is_some();
        self.shot(cx, "outline-tab").await;

        // Quick open's `@`, typed: a class of the other module.
        if let Some((_, at)) = self.glyph(cx, "def make_order") {
            self.click(cx, at).await;
        }
        self.keys(cx, "cmd-p").await;
        self.pause(cx, 300).await;
        self.type_text(cx, "@Order").await;
        let rows = |shell: &Shell, cx: &App| -> Vec<(String, String)> {
            (shell.quick_open.as_ref())
                .map(|open| open.picker().read(cx).row_labels())
                .unwrap_or_default()
        };
        let mut listed = Vec::new();
        for _ in 0..50 {
            listed = self
                .shell(cx, |shell, _, cx| rows(shell, cx))
                .unwrap_or_default();
            if listed.iter().any(|(title, _)| title == "Order") {
                break;
            }
            self.pause(cx, 100).await;
        }
        self.shot(cx, "quick-open-symbols").await;
        let found = listed.iter().position(|(title, _)| title == "Order");
        // Down to its row, then Enter.
        for _ in 0..found.unwrap_or(0) {
            self.keys(cx, "down").await;
        }
        self.keys(cx, "enter").await;
        let target = self.path("shop/models.py").to_string_lossy().into_owned();
        let opened = self
            .until(cx, 5000, |_, cx| {
                context::focused_file(cx).is_some_and(|file| file.path == target)
            })
            .await;
        self.pause(cx, 400).await;
        let place = self.place(cx);
        let class_line = self.row(cx, "class Order").map(|row| row + 1);
        let symbol_ok = opened
            && place
                .as_ref()
                .is_some_and(|(_, line, _)| Some(*line) == class_line);
        self.record(
            "6",
            "the Outline tab lists the file's symbols and @ in quick open finds a class of another file",
            revealed && outline_ok && outline_drawn && found.is_some() && symbol_ok,
            json!({
                "outline": names, "outline_drawn": outline_drawn,
                "quick_open_rows": listed, "opened_at": place, "class_line": class_line,
            }),
        );
        self.keys(cx, "escape").await;
        self.open(cx, ORDERS).await;
    }
}
