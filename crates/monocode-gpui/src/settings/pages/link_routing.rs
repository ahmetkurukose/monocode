//! Settings → Browser → Link routing (docs/native/modes.md §Link routing):
//! the user's rules, as SOCode's Air Traffic Control sheet. Each sends the
//! links it matches (a site, an address containing some text, or one
//! address) to Browser mode, a project, the Little window or the system
//! browser; the first enabled rule that matches wins. A rule is made in the
//! row at the end of the list.
use crate::{
    input::{self, Input},
    settings::{
        SettingsView,
        widgets::{icon_action, secondary_button},
    },
    shell::browser_links::{LinkRoute, LinkTarget, add_route, link_match},
    theme::*,
};
use gpui::{AnyElement, Context, Entity, SharedString, div, prelude::*, px};
use socode_browser::routing::LinkMatch;
use std::{cell::RefCell, collections::HashMap};

const FIELD: &str = "link-route-value";
const KINDS: [(&str, &str); 3] = [
    ("domain", "Site"),
    ("contains", "Contains"),
    ("equal", "Is"),
];

/// The rule being made: how it matches and where it sends links.
#[derive(Clone, Debug)]
pub(in crate::settings) struct LinkDraft {
    kind: &'static str,
    target: String,
}

impl Default for LinkDraft {
    fn default() -> Self {
        Self {
            kind: "domain",
            target: target_value(&LinkTarget::Little),
        }
    }
}

/// A select's value for `target`.
fn target_value(target: &LinkTarget) -> String {
    match target {
        LinkTarget::Browser => "browser".into(),
        LinkTarget::Little => "little".into(),
        LinkTarget::System => "system".into(),
        LinkTarget::Project(path) => format!("project:{path}"),
    }
}

fn target_of(value: &str) -> LinkTarget {
    match value {
        "browser" => LinkTarget::Browser,
        "system" => LinkTarget::System,
        _ => match value.strip_prefix("project:") {
            Some(path) => LinkTarget::Project(path.to_owned()),
            None => LinkTarget::Little,
        },
    }
}

/// What a rule matches, for its row.
fn describe(condition: &LinkMatch) -> (String, &'static str) {
    match condition {
        LinkMatch::Domain(value) => (value.clone(), "The site and its subdomains"),
        LinkMatch::Contains(value) => (value.clone(), "Addresses containing this"),
        LinkMatch::IsEqualTo(value) => (value.clone(), "This address only"),
        LinkMatch::SourceApplication(value) => (value.clone(), "Links from this app"),
    }
}

/// A `&'static str` id for a rule's control: the settings widgets key their
/// state by static ids. Each rule's ids are made once.
fn rule_id(what: &'static str, id: u64) -> &'static str {
    thread_local! {
        static IDS: RefCell<HashMap<(&'static str, u64), &'static str>> = RefCell::default();
    }
    IDS.with(|ids| {
        *(ids.borrow_mut().entry((what, id)))
            .or_insert_with(|| Box::leak(format!("{what}-{id}").into_boxed_str()))
    })
}

impl SettingsView {
    /// The Link routing group of the Browser page.
    pub(super) fn link_routing_group(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let routes = self.prefs.browser_link_routes.clone();
        let mut rows: Vec<AnyElement> = (routes.iter().enumerate())
            .map(|(ix, route)| self.link_route_row(ix, route, cx))
            .collect();
        rows.push(self.link_route_draft_row(routes.is_empty(), cx));
        self.group(
            Some("browser-link-routing"),
            false,
            "Link routing",
            Some(
                "Where links open, by site. The first rule that matches wins. Without one, links from other apps open in the Little window, and links in Monocode follow “Open all links in Monocode”."
                    .into(),
            ),
            None,
            rows,
        )
    }

    fn target_options(&self) -> Vec<(String, SharedString, Option<AnyElement>)> {
        let mut options = vec![
            ("browser".to_owned(), "Browser mode".into(), None),
            ("little".to_owned(), "Little window".into(), None),
            ("system".to_owned(), "System browser".into(), None),
        ];
        options.extend(self.ctx.projects.iter().map(|(path, label)| {
            (
                format!("project:{path}"),
                SharedString::from(label.clone()),
                Some(icon("folder", 14.).into_any_element()),
            )
        }));
        options
    }

    fn link_route_row(
        &mut self,
        ix: usize,
        route: &LinkRoute,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = route.id;
        let (value, what) = describe(&route.condition);
        let mut target = target_value(&route.target);
        let mut options = self.target_options();
        if let LinkTarget::Project(path) = &route.target
            && !options.iter().any(|(v, ..)| *v == target)
        {
            // A project no longer in the workspace: shown, and falls back
            // to the clicked window's project.
            options.push((target.clone(), format!("{path} (missing)").into(), None));
        }
        if options.is_empty() {
            target.clear();
        }
        let enabled = self.toggle(
            rule_id("link-route-on", id),
            "Rule on",
            route.enabled,
            false,
            cx,
            move |this, on, cx| {
                this.update(cx, |p| {
                    if let Some(route) = p.browser_link_routes.iter_mut().find(|r| r.id == id) {
                        route.enabled = on;
                    }
                })
            },
        );
        let select = self.select(
            rule_id("link-route-target", id),
            "Opens in",
            &target,
            options,
            Some(160.),
            cx,
            move |this, value, cx| {
                this.update(cx, |p| {
                    if let Some(route) = p.browser_link_routes.iter_mut().find(|r| r.id == id) {
                        route.target = target_of(&value);
                    }
                })
            },
        );
        let up = icon_action(
            SharedString::from(rule_id("link-route-up", id)),
            "Move up",
            "arrow-up",
            24.,
            13.,
            0.5,
            ix == 0,
        )
        .debug_selector(move || format!("link-route-up-{id}"))
        .when(ix > 0, |d| {
            d.on_click(cx.listener(move |this, _, _, cx| {
                this.update(cx, |p| p.browser_link_routes.swap(ix, ix - 1))
            }))
        })
        .into_any_element();
        let remove = icon_action(
            SharedString::from(rule_id("link-route-remove", id)),
            "Remove rule",
            "trash",
            24.,
            13.,
            0.5,
            false,
        )
        .debug_selector(move || format!("link-route-remove-{id}"))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.update(cx, |p| p.browser_link_routes.retain(|r| r.id != id))
        }))
        .into_any_element();
        self.row(
            None,
            ix == 0,
            div()
                .debug_selector(move || format!("link-route-{id}"))
                .child(value),
            Some(what.into()),
            vec![enabled, select, up, remove],
        )
    }

    fn link_route_field(&mut self, cx: &mut Context<Self>) -> Entity<Input> {
        if let Some(input) = self.fields.get(FIELD) {
            return input.clone();
        }
        let input = cx.new(|cx| {
            let mut input = Input::field("Site or address", "github.com", content(0.35), cx);
            input.set_font_size(12.);
            input
        });
        self._subscriptions
            .push(cx.subscribe(&input, |_, _, _: &input::Changed, cx| cx.notify()));
        self._subscriptions
            .push(cx.subscribe(&input, |this, _, _: &input::Submitted, cx| {
                this.add_link_route(cx)
            }));
        self.fields.insert(FIELD, input.clone());
        input
    }

    /// The draft's rule joins the end of the list.
    pub(in crate::settings) fn add_link_route(&mut self, cx: &mut Context<Self>) {
        let input = self.link_route_field(cx);
        let text = input.read(cx).content.clone();
        let Some(condition) = link_match(self.link_draft.kind, &text) else {
            return;
        };
        let target = target_of(&self.link_draft.target);
        self.update(cx, |p| {
            add_route(&mut p.browser_link_routes, condition, target);
        });
        input.update(cx, |input, cx| input.set_text(String::new(), cx));
    }

    fn link_route_draft_row(&mut self, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let input = self.link_route_field(cx);
        let empty = input.read(cx).content.trim().is_empty();
        let kind = self.link_draft.kind;
        let kinds = self.segmented(
            "link-route-kind",
            "Match",
            kind,
            &KINDS,
            cx,
            |this, kind, cx| {
                this.link_draft.kind = kind;
                cx.notify();
            },
        );
        let target = self.link_draft.target.clone();
        let options = self.target_options();
        let select = self.select(
            "link-route-new-target",
            "Opens in",
            &target,
            options,
            Some(160.),
            cx,
            |this, value, cx| {
                this.link_draft.target = value;
                cx.notify();
            },
        );
        let field = div()
            .debug_selector(|| "link-route-field".into())
            .h(px(28.))
            .w(px(180.))
            .rounded_md()
            .border_1()
            .border_color(content(0.12))
            .bg(content(0.03))
            .px_2()
            .flex()
            .items_center()
            .child(input)
            .into_any_element();
        let add = secondary_button("link-route-add", "Add Rule", false, empty)
            .debug_selector(|| "link-route-add".into())
            .when(!empty, |d| {
                d.on_click(cx.listener(|this, _, _, cx| this.add_link_route(cx)))
            })
            .into_any_element();
        self.row(
            Some("browser-link-routing-new"),
            first,
            "New rule",
            Some("A site such as github.com (with its subdomains), text the address contains, or one whole address.".into()),
            vec![kinds, field, select, add],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{FIELD, target_of, target_value};
    use crate::settings::{Preferences, Section, SettingsContext, SettingsView};
    use crate::shell::browser_links::LinkTarget;
    use gpui::{
        AppContext, Bounds, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point,
        px, size,
    };
    use socode_browser::routing::LinkMatch;

    fn click(visual: &mut VisualTestContext, selector: &'static str) {
        let at = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not drawn"))
            .center();
        visual.simulate_click(at, Default::default());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn rules_are_made_switched_off_and_removed(cx: &mut TestAppContext) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(1200.), px(6000.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|cx| {
                        let mut view = SettingsView::new(
                            Section::Browser,
                            Preferences::default(),
                            SettingsContext::default(),
                            cx,
                        );
                        view.discovery_task = None;
                        view.discovery_loading = false;
                        view
                    })
                },
            )
            .unwrap()
        });
        cx.run_until_parked();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        assert!(visual.debug_bounds("link-route-field").is_some());
        window
            .update(visual, |view, _, cx| {
                let input = view.fields[FIELD].clone();
                input.update(cx, |input, cx| {
                    input.set_text("https://github.com/a".into(), cx)
                });
            })
            .unwrap();
        visual.run_until_parked();
        click(visual, "link-route-add");
        let routes = |visual: &mut VisualTestContext| {
            window
                .update(visual, |view, _, _| view.prefs.browser_link_routes.clone())
                .unwrap()
        };
        let made = routes(visual);
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].condition, LinkMatch::Domain("github.com".into()));
        assert_eq!(made[0].target, LinkTarget::Little);
        assert!(made[0].enabled);
        assert!(visual.debug_bounds("link-route-1").is_some());
        click(visual, "settings-toggle-link-route-on-1");
        assert!(!routes(visual)[0].enabled);
        click(visual, "link-route-remove-1");
        assert!(routes(visual).is_empty());
    }

    #[test]
    fn a_target_survives_its_select_value() {
        for target in [
            LinkTarget::Browser,
            LinkTarget::Little,
            LinkTarget::System,
            LinkTarget::Project("/Users/me/app".into()),
        ] {
            assert_eq!(target_of(&target_value(&target)), target);
        }
    }
}
