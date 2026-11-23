//! Settings → Extensions (docs/native/browser/context.md K69, K70): add an
//! extension from the Chrome Web Store by its link or id (Proton Pass one
//! click away), and the installed ones with what they may do, on/off,
//! updates waiting for approval, and removal. Extensions are installed for
//! the app; each browser profile runs them with data of its own.
use crate::{
    browser::extensions::{self, Work},
    input::{self, Input},
    settings::{
        SettingsView,
        widgets::{note, primary_button, red, secondary_button},
    },
    theme::*,
};
use gpui::{AnyElement, Context, ElementId, Entity, SharedString, div, img, prelude::*, px};
use monocode_core::browser::extensions::PROTON_PASS_ID;

#[derive(Default)]
pub(in crate::settings) struct ExtensionsState {
    field: Option<Entity<Input>>,
    /// The extension whose removal waits for a second click.
    confirm_remove: Option<String>,
    /// Why the last add did not start.
    error: Option<String>,
}

impl SettingsView {
    fn extension_field(&mut self, cx: &mut Context<Self>) -> Entity<Input> {
        if let Some(field) = &self.extensions.field {
            return field.clone();
        }
        let field = cx.new(|cx| {
            let mut field = Input::field(
                "Chrome Web Store link or extension id",
                "https://chromewebstore.google.com/detail/…",
                content(0.35),
                cx,
            );
            field.set_font_size(13.);
            field
        });
        self._subscriptions
            .push(cx.subscribe(&field, |_, _, _: &input::Changed, cx| cx.notify()));
        self._subscriptions.push(
            cx.subscribe(&field, |this, field, _: &input::Submitted, cx| {
                let text = field.read(cx).content.to_string();
                this.add_extension(&text, cx);
            }),
        );
        self.extensions.field = Some(field.clone());
        field
    }

    fn add_extension(&mut self, input: &str, cx: &mut Context<Self>) {
        if let Err(e) = extensions::install(input) {
            self.extensions.error = Some(e);
        } else {
            self.extensions.error = None;
            if let Some(field) = &self.extensions.field {
                field.update(cx, |f, cx| f.set_text(String::new(), cx));
            }
        }
        cx.notify();
    }

    pub(in crate::settings) fn extensions_page(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let field = self.extension_field(cx);
        let listings = extensions::listings();
        let work = extensions::work();
        let busy = matches!(work, Work::Busy(_));
        let has_proton = listings.iter().any(|l| l.id == PROTON_PASS_ID);
        let typed = field.read(cx).content.trim().to_owned();
        let status: Option<AnyElement> = match (&self.extensions.error, &work) {
            (Some(e), _) => Some(status_line(e.clone(), true)),
            (None, Work::Busy(t)) => Some(status_line(t.clone(), false)),
            (None, Work::Done(t)) => Some(status_line(t.clone(), false)),
            (None, Work::Failed(t)) => Some(status_line(t.clone(), true)),
            (None, Work::Idle) => None,
        };
        let add_row = div()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(32.))
                            .rounded_lg()
                            .border_1()
                            .border_color(content(0.15))
                            .bg(content(0.03))
                            .px_3()
                            .flex()
                            .items_center()
                            .child(field.clone()),
                    )
                    .child(
                        primary_button("extensions-add", "Add", busy || typed.is_empty())
                            .child("Add")
                            .when(!busy && !typed.is_empty(), |b| {
                                b.on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.add_extension(&typed, cx)
                                    }),
                                )
                            }),
                    ),
            )
            .when(!has_proton, |d| {
                d.child(
                    div().flex().items_center().gap_2().child(
                        secondary_button("extensions-add-proton", "Add Proton Pass", false, busy)
                            .child(icon("key", 12.))
                            .child("Add Proton Pass")
                            .when(!busy, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| {
                                    this.add_extension(PROTON_PASS_ID, cx)
                                }))
                            }),
                    ),
                )
            })
            .children(status)
            .into_any_element();
        let mut page = vec![self.group(
            Some("browser-extensions-add"),
            true,
            "Add extension",
            Some("Extensions come from the Chrome Web Store, verified as Chrome verifies them, and update on their own. Each browser profile runs them with its own data and sign-ins; agents' throwaway profiles run none. Agents cannot add or remove extensions.".into()),
            None,
            vec![add_row],
        )];
        let check = secondary_button(
            "extensions-check-updates",
            "Check for updates",
            false,
            busy || listings.is_empty(),
        )
        .child("Check for updates")
        .when(!busy && !listings.is_empty(), |b| {
            b.on_click(cx.listener(|_, _, _, cx| {
                extensions::check_updates(true);
                cx.notify();
            }))
        })
        .into_any_element();
        let mut rows = Vec::new();
        if listings.is_empty() {
            rows.push(note("No extensions yet. Add one above."));
        }
        for (ix, l) in listings.into_iter().enumerate() {
            rows.push(self.extension_row(ix, l, cx));
        }
        page.push(self.group(
            Some("browser-extensions-installed"),
            false,
            "Installed",
            None,
            Some(check),
            rows,
        ));
        page
    }

    fn extension_row(
        &mut self,
        ix: usize,
        l: extensions::Listing,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = l.id.clone();
        let confirming = self.extensions.confirm_remove.as_deref() == Some(id.as_str());
        let logo: AnyElement = match &l.icon {
            Some(path) => img(path.clone())
                .size(px(28.))
                .flex_shrink_0()
                .into_any_element(),
            None => div()
                .size(px(28.))
                .rounded_md()
                .bg(content(0.1))
                .flex_shrink_0()
                .into_any_element(),
        };
        let toggle_id = id.clone();
        let on = l.enabled;
        let toggle = div()
            .id(ElementId::NamedInteger(
                "extension-toggle".into(),
                ix as u64,
            ))
            .role(gpui::Role::Switch)
            .aria_label(SharedString::from(format!("Enable {}", l.name)))
            .aria_toggled(if on {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .relative()
            .h(px(20.))
            .w(px(36.))
            .flex_shrink_0()
            .rounded_full()
            .cursor_pointer()
            .bg(if on { accent_color() } else { content(0.2) })
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(if on { 18. } else { 2. }))
                    .size(px(16.))
                    .rounded_full()
                    .bg(gpui::rgb(0xffffff)),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                extensions::set_enabled(&toggle_id, !on);
                cx.notify();
            }));
        let remove_id = id.clone();
        let remove = secondary_button(
            ElementId::NamedInteger("extension-remove".into(), ix as u64),
            if confirming {
                "Remove with its data"
            } else {
                "Remove"
            },
            true,
            false,
        )
        .child(if confirming {
            "Remove with its data"
        } else {
            "Remove"
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            if this.extensions.confirm_remove.as_deref() == Some(remove_id.as_str()) {
                this.extensions.confirm_remove = None;
                extensions::uninstall(&remove_id);
            } else {
                this.extensions.confirm_remove = Some(remove_id.clone());
            }
            cx.notify();
        }));
        let held = l.held.clone().map(|h| {
            let approve_id = id.clone();
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(12.))
                .text_color(content(0.75))
                .child(format!(
                    "Version {} asks for more: {}.",
                    h.version,
                    h.warnings.join("; ")
                ))
                .child(
                    secondary_button(
                        ElementId::NamedInteger("extension-approve".into(), ix as u64),
                        "Allow and update",
                        false,
                        false,
                    )
                    .child("Allow and update")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        extensions::approve_update(&approve_id);
                        cx.notify();
                    })),
                )
        });
        let small = |text: String, alpha: f32| {
            div()
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(content(alpha))
                .child(text)
        };
        div()
            .px_4()
            .py(px(12.))
            .flex()
            .items_start()
            .gap_3()
            .border_t_1()
            .border_color(content(0.06))
            .child(logo)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_size(px(13.)).text_color(content(0.95)).child(l.name.clone()))
                            .child(small(l.version.clone(), 0.45)),
                    )
                    .when(!l.description.is_empty(), |d| d.child(small(l.description.clone(), 0.6)))
                    .when(!l.warnings.is_empty(), |d| {
                        d.child(small(format!("May: {}.", l.warnings.join("; ").to_lowercase()), 0.5))
                    })
                    .when(!l.unsupported.is_empty(), |d| {
                        d.child(small(
                            format!("Not available in Monocode's browser: {}.", l.unsupported.join(", ")),
                            0.5,
                        ))
                    })
                    .when(l.id == PROTON_PASS_ID && l.enabled, |d| {
                        d.child(small(
                            "While it is on, Monocode's own password manager neither saves nor fills; Proton Pass does.".into(),
                            0.5,
                        ))
                    })
                    .children(held),
            )
            .child(div().flex().items_center().gap_2().flex_shrink_0().child(toggle).child(remove))
            .into_any_element()
    }
}

fn status_line(text: String, error: bool) -> AnyElement {
    div()
        .text_size(px(12.))
        .line_height(px(17.))
        .text_color(if error { red(1.) } else { content(0.6) })
        .child(text)
        .into_any_element()
}
