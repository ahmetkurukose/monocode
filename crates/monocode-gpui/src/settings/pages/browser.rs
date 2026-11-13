//! BrowserPage: what agents may do in the browser pane, when loaded pages
//! give their memory back, where links open, and what sites have stored
//! (docs/native/browser.md §3.5, §3.6, §3.9).
use crate::settings::{SettingsEvent, SettingsView, widgets::secondary_button};
use crate::theme::*;
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};

/// What the Browser page shows of the data stored by sites.
#[derive(Default)]
pub(crate) struct BrowserData {
    /// Sites with stored data, each with what kinds; `None` until asked for.
    pub(crate) sites: Option<Vec<(String, Vec<String>)>>,
    pub(crate) loading: bool,
    /// What the last clearing did, said under the buttons.
    pub(crate) status: Option<String>,
}

impl SettingsView {
    fn load_browser_sites(&mut self, cx: &mut Context<Self>) {
        self.browser_data.loading = true;
        let (view, app) = (cx.weak_entity(), cx.to_async());
        crate::browser::data_sites(move |sites| {
            app.spawn(async move |cx| {
                let _ = view.update(cx, |this, cx| {
                    this.browser_data.loading = false;
                    this.browser_data.sites = Some(sites);
                    cx.notify();
                });
            })
            .detach();
        });
        cx.notify();
    }

    fn clear_browser_data(
        &mut self,
        what: &'static str,
        seconds: Option<f64>,
        said: &'static str,
        cx: &mut Context<Self>,
    ) {
        let (view, app) = (cx.weak_entity(), cx.to_async());
        crate::browser::clear_data(what, seconds, move || {
            app.spawn(async move |cx| {
                let _ = view.update(cx, |this, cx| {
                    this.browser_data.status = Some(said.into());
                    // What was listed may be gone now.
                    if this.browser_data.sites.is_some() {
                        this.load_browser_sites(cx);
                    }
                    cx.notify();
                });
            })
            .detach();
        });
    }

    fn clear_browser_site(&mut self, site: String, cx: &mut Context<Self>) {
        let (view, app) = (cx.weak_entity(), cx.to_async());
        let name = site.clone();
        crate::browser::clear_site(site, move |_| {
            app.spawn(async move |cx| {
                let _ = view.update(cx, |this, cx| {
                    this.browser_data.status = Some(format!("Cleared what {name} stored."));
                    this.load_browser_sites(cx);
                });
            })
            .detach();
        });
    }

    pub(super) fn browser_page(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = self.prefs.clone();
        let agents = vec![self.row(
            Some("browser-agents"),
            true,
            "Agents in the browser",
            Some("What an agent may do with the browser pane of its session's project. With Any site it goes anywhere, signed in as you are. A page shows who is using it, with a Stop button. Applies to sessions started afterwards.".into()),
            vec![self.segmented(
                "browser-agents",
                "Agents in the browser",
                &p.browser_agents,
                &[("any", "Any site"), ("local", "Local addresses"), ("off", "Off")],
                cx,
                |this, value, cx| this.update(cx, |p| p.browser_agents = value.into()),
            )],
        )];
        let links = vec![self.row(
            Some("browser-links"),
            true,
            "Links open in",
            Some("Where a web link in a reply or a terminal opens.".into()),
            vec![self.segmented(
                "browser-links",
                "Links open in",
                &p.browser_links,
                &[("monocode", "Monocode"), ("system", "My browser")],
                cx,
                |this, value, cx| this.update(cx, |p| p.browser_links = value.into()),
            )],
        )];
        let minutes = p.browser_sleep_minutes.to_string();
        let live = p.browser_max_live.to_string();
        let memory = vec![
            self.row(
                Some("browser-sleep"),
                true,
                "Sleep pages off screen after",
                Some("A sleeping page gives back its memory and keeps its address; showing it loads it again. Pages you keep awake, and pages on screen, do not sleep.".into()),
                vec![self.segmented(
                    "browser-sleep",
                    "Sleep pages off screen after",
                    &minutes,
                    &[("5", "5 min"), ("15", "15 min"), ("60", "1 hour"), ("0", "Never")],
                    cx,
                    |this, value, cx| {
                        if let Ok(minutes) = value.parse() {
                            this.update(cx, |p| p.browser_sleep_minutes = minutes);
                        }
                    },
                )],
            ),
            self.row(
                Some("browser-max-live"),
                false,
                "Pages loaded at once",
                Some("With more than this loaded, the ones unused longest sleep. Short of memory, every page off screen sleeps whatever this says.".into()),
                vec![self.segmented(
                    "browser-max-live",
                    "Pages loaded at once",
                    &live,
                    &[("2", "2"), ("4", "4"), ("8", "8"), ("16", "16")],
                    cx,
                    |this, value, cx| {
                        if let Ok(pages) = value.parse() {
                            this.update(cx, |p| p.browser_max_live = pages);
                        }
                    },
                )],
            ),
            self.row(
                Some("browser-pages"),
                false,
                "Open pages",
                Some("Sleep frees every page's memory and keeps its pane. Close removes the panes too.".into()),
                vec![
                    secondary_button("browser-sleep-all", "Sleep all", false, false)
                        .child("Sleep all")
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(SettingsEvent::BrowserPages("sleep"))
                        }))
                        .into_any_element(),
                    secondary_button("browser-close-all", "Close all", true, false)
                        .child("Close all")
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(SettingsEvent::BrowserPages("close"))
                        }))
                        .into_any_element(),
                ],
            ),
        ];
        let hour = Some(3600.);
        let clear = |this: &Self,
                     cx: &mut Context<Self>,
                     id: &'static str,
                     label: &'static str,
                     danger: bool,
                     what: &'static str,
                     seconds: Option<f64>,
                     said: &'static str| {
            let _ = this;
            secondary_button(id, label, danger, false)
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.clear_browser_data(what, seconds, said, cx)
                }))
                .into_any_element()
        };
        let mut data = vec![
            self.row(
                Some("browser-clear-cache"),
                true,
                "Cache",
                Some("Copies of pages, scripts and images kept to load faster. Clearing it signs you out of nothing.".into()),
                vec![clear(self, cx, "browser-clear-cache", "Clear cache", false, "cache", None, "The cache is cleared.")],
            ),
            self.row(
                Some("browser-clear-site-data"),
                false,
                "Cookies and site data",
                Some("Sign-ins and whatever sites stored. Clearing it signs you out everywhere in this browser.".into()),
                vec![
                    clear(self, cx, "browser-clear-recent", "Last hour", true, "site-data", hour, "Cookies and site data of the last hour are cleared."),
                    clear(self, cx, "browser-clear-all-data", "Clear all", true, "site-data", None, "Cookies and site data are cleared."),
                ],
            ),
        ];
        let status: Option<SharedString> = self.browser_data.status.clone().map(Into::into);
        if let Some(status) = status {
            data.push(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(12.))
                    .text_color(content(0.6))
                    .child(status)
                    .into_any_element(),
            );
        }
        let sites_action = secondary_button(
            "browser-sites-load",
            if self.browser_data.loading {
                "Reading…"
            } else {
                "Show sites"
            },
            false,
            self.browser_data.loading,
        )
        .child(if self.browser_data.loading {
            "Reading…"
        } else {
            "Show sites"
        })
        .on_click(cx.listener(|this, _, _, cx| this.load_browser_sites(cx)))
        .into_any_element();
        let mut sites: Vec<AnyElement> = Vec::new();
        match &self.browser_data.sites {
            None => {}
            Some(list) if list.is_empty() => sites.push(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(12.))
                    .text_color(content(0.5))
                    .child("No site has stored anything.")
                    .into_any_element(),
            ),
            Some(list) => {
                for (ix, (site, kinds)) in list.iter().take(300).enumerate() {
                    let name = site.clone();
                    sites.push(
                        div()
                            .px_3()
                            .py(px(6.))
                            .flex()
                            .items_center()
                            .gap_3()
                            .when(ix > 0, |d| d.border_t_1().border_color(content(0.06)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .truncate()
                                            .child(SharedString::from(site.clone())),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(content(0.45))
                                            .truncate()
                                            .child(SharedString::from(kinds.join(", "))),
                                    ),
                            )
                            .child(
                                secondary_button(("browser-site-clear", ix), "Remove", true, false)
                                    .child("Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.clear_browser_site(name.clone(), cx)
                                    })),
                            )
                            .into_any_element(),
                    );
                }
                if list.len() > 300 {
                    sites.push(
                        div()
                            .px_3()
                            .py_2()
                            .text_size(px(12.))
                            .text_color(content(0.5))
                            .child(format!("…and {} more", list.len() - 300))
                            .into_any_element(),
                    );
                }
            }
        }
        vec![
            self.group(None, true, "Agents", None, None, agents),
            self.group(None, false, "Links", None, None, links),
            self.group(None, false, "Memory", None, None, memory),
            self.group(
                None,
                false,
                "Stored data",
                Some("The browser keeps its data apart from your own browser's, in Monocode's own WebKit store.".into()),
                None,
                data,
            ),
            self.group(
                None,
                false,
                "Sites",
                Some("Each site that stored something, and what kinds. Removing one signs you out of it.".into()),
                Some(sites_action),
                sites,
            ),
        ]
    }
}
