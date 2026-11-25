//! Browser mode's media card (docs/native/modes.md, "Media and
//! Picture-in-Picture"): a strip at the sidebar's foot for a tab that
//! plays while it is not on screen. Its row holds Back to Tab (the tab's
//! icon), Previous Track, Play/Pause, Next Track and the tab's mute; under
//! the pointer a header rises over the list with the tab's title, Open
//! Picture in Picture and a close button that dismisses the card while
//! the media plays on. As the reference: one card, for the tab that
//! started playing last; it leaves when the pointer has been gone for two
//! seconds. The page side is `crate::browser::playback`.
use super::*;
use crate::browser::{BrowserPage, MediaReturn};
use monocode_core::browser::playback::{Action, Playback};
use std::time::Duration;

/// The card's row and its header (the reference's 33 points each).
const ROW: f32 = 33.;
/// How long the header takes to rise or fall.
const REVEAL: Duration = Duration::from_millis(200);
/// How long the header stays once the pointer left.
const LINGER: Duration = Duration::from_secs(2);

/// The card's state, kept by the shell.
#[derive(Default)]
pub(super) struct MediaCard {
    /// The pane the card shows, while it can.
    pane: Option<i64>,
    /// The panes playing at the last look: one that starts takes the card.
    playing: HashSet<i64>,
    /// Closed with the card's ✕: they come back when they play again.
    dismissed: HashSet<i64>,
    hovered: bool,
    /// How far the header has risen, 0 to 1.
    header: f32,
    generation: u64,
    animation: Option<gpui::Task<()>>,
    collapse: Option<gpui::Task<()>>,
}

/// The pane whose media the card shows: the one it showed, if it still
/// can, else the first one that can. A pane can while it is active, off
/// screen and not dismissed.
fn source(
    card: &MediaCard,
    candidates: &[(i64, Playback)],
    on_screen: &[i64],
) -> Option<(i64, Playback)> {
    let eligible = |(pane, state): &&(i64, Playback)| {
        state.active && !on_screen.contains(pane) && !card.dismissed.contains(pane)
    };
    (card.pane)
        .and_then(|pane| candidates.iter().filter(eligible).find(|(p, _)| *p == pane))
        .or_else(|| candidates.iter().find(eligible))
        .cloned()
}

impl MediaCard {
    /// Notes what each page plays: a pane that starts playing takes the
    /// card and comes back if it was dismissed; one that stopped having
    /// media is no longer dismissed.
    fn record(&mut self, candidates: &[(i64, Playback)]) {
        let playing: HashSet<i64> = (candidates.iter())
            .filter(|(_, state)| state.playing)
            .map(|(pane, _)| *pane)
            .collect();
        for (pane, _) in candidates.iter().filter(|(_, state)| state.playing) {
            if !self.playing.contains(pane) {
                self.pane = Some(*pane);
                self.dismissed.remove(pane);
            }
        }
        self.playing = playing;
        self.dismissed
            .retain(|pane| candidates.iter().any(|(p, s)| p == pane && s.active));
    }
}

/// A tooltip of the card's controls.
struct MediaTip(SharedString);

impl Render for MediaTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(pane_fill())
            .border_1()
            .border_color(content(0.15))
            .text_size(px(12.))
            .text_color(content(1.))
            .child(self.0.clone())
    }
}

/// Play, pause, previous and next, drawn as the reference draws them.
fn transport(action: &'static str) -> AnyElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let color = window.text_style().color;
            let at = |x: f32, y: f32| bounds.origin + gpui::point(px(x), px(y));
            if action == "pause" {
                for x in [5.75, 11.25] {
                    window.paint_quad(
                        gpui::fill(Bounds::new(at(x, 4.), gpui::size(px(3.5), px(12.))), color)
                            .corner_radii(px(1.)),
                    );
                }
                return;
            }
            let skip = action != "play";
            let back = action == "previous";
            let offset = match (skip, back) {
                (false, _) => 0.,
                (true, true) => 1.,
                (true, false) => -1.,
            };
            let place = |x: f32, y: f32| {
                at(
                    (if back { 20. - x } else { x }) + offset,
                    if skip { 4. + (y - 4.) * 11. / 12. } else { y },
                )
            };
            let (left, right) = if skip { (3.5, 14.) } else { (6., 16.) };
            let mut path = gpui::PathBuilder::fill();
            path.move_to(place(left, 4.8));
            path.curve_to(place(left + 1.4, 4.1), place(left, 3.4));
            path.line_to(place(right - 0.4, 9.2));
            path.curve_to(place(right - 0.4, 10.8), place(right + 1., 10.));
            path.line_to(place(left + 1.4, 15.9));
            path.curve_to(place(left, 15.2), place(left, 16.6));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
            if skip {
                let x = if back { 3. } else { 14. };
                window.paint_quad(
                    gpui::fill(
                        Bounds::new(at(x + offset, 4.), gpui::size(px(3.), px(11.))),
                        color,
                    )
                    .corner_radii(px(1.)),
                );
            }
        },
    )
    .size(px(20.))
    .flex_none()
    .into_any_element()
}

/// Two windows, the smaller over the larger's corner: Open Picture in
/// Picture.
fn pip_glyph() -> AnyElement {
    canvas(
        |_, _, _| (),
        |bounds, _, window, _| {
            let color = window.text_style().color;
            let at = |x: f32, y: f32| bounds.origin + gpui::point(px(x), px(y));
            let mut back = gpui::PathBuilder::stroke(px(1.15));
            back.move_to(at(7., 11.));
            back.line_to(at(3., 11.));
            back.curve_to(at(1.5, 9.5), at(1.5, 11.));
            back.line_to(at(1.5, 4.));
            back.curve_to(at(3., 2.5), at(1.5, 2.5));
            back.line_to(at(11.5, 2.5));
            back.curve_to(at(13., 4.), at(13., 2.5));
            back.line_to(at(13., 7.));
            if let Ok(path) = back.build() {
                window.paint_path(path, color);
            }
            window.paint_quad(gpui::quad(
                Bounds::new(at(7., 7.), gpui::size(px(9.5), px(7.5))),
                px(1.5),
                gpui::transparent_black(),
                px(1.15),
                color,
                gpui::BorderStyle::Solid,
            ));
        },
    )
    .size(px(18.))
    .flex_none()
    .into_any_element()
}

impl Shell {
    /// The pages that have media, by pane, and what they play.
    fn browse_media_candidates(&self, cx: &App) -> Vec<(i64, Playback)> {
        let mut out: Vec<(i64, Playback)> = (self.browser_pages.iter())
            .filter(|(pane, _)| self.browse_exists(**pane))
            .filter_map(|(pane, (page, _))| Some((*pane, page.read(cx).playback()?)))
            .collect();
        out.sort_by_key(|(pane, _)| *pane);
        out
    }

    /// The pane the media card is for, and what it plays.
    fn browse_media_source(&self, cx: &App) -> Option<(i64, Playback)> {
        let on_screen = if self.browser_mode() {
            self.browse_split_panes().0
        } else {
            Vec::new()
        };
        source(
            &self.browse_media,
            &self.browse_media_candidates(cx),
            &on_screen,
        )
    }

    /// Shows the tab of `pane`: Back to Tab, from the card or the PiP
    /// window.
    pub(super) fn show_media_tab(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.workspace.browser_panes.contains_key(&pane) {
            return;
        }
        if self.workspace.browse.owns(pane) {
            self.browse_select(pane, window, cx);
        } else {
            self.open_browser_pane(pane, window, cx);
        }
        let card = &mut self.browse_media;
        card.hovered = false;
        card.header = 0.;
        card.animation = None;
        card.collapse = None;
        cx.notify();
    }

    /// A page's PiP window went back to its tab: show it.
    pub(super) fn watch_media_return(
        &mut self,
        pane: i64,
        page: &Entity<BrowserPage>,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe(page, move |_, _, _: &MediaReturn, cx| {
            cx.spawn(async move |this, cx| {
                let _ =
                    this.update_in(cx, |this, window, cx| this.show_media_tab(pane, window, cx));
            })
            .detach();
        })
        .detach();
    }

    fn media_page(&self, pane: i64) -> Option<Entity<BrowserPage>> {
        self.browser_pages.get(&pane).map(|(page, _)| page.clone())
    }

    /// Moves the header to `to` (0 down, 1 risen), over [`REVEAL`].
    fn animate_media_header(&mut self, to: f32, cx: &mut Context<Self>) {
        let card = &mut self.browse_media;
        let from = card.header;
        card.animation = None;
        card.generation = card.generation.wrapping_add(1);
        let generation = card.generation;
        if cx.reduce_motion() || from == to {
            card.header = to;
            cx.notify();
            return;
        }
        let started = std::time::Instant::now();
        card.animation = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
                let t = (started.elapsed().as_secs_f32() / REVEAL.as_secs_f32()).min(1.);
                let eased = 1. - (1. - t).powi(3);
                let more = this
                    .update(cx, |this, cx| {
                        let card = &mut this.browse_media;
                        if card.generation != generation {
                            return false;
                        }
                        card.header = from + (to - from) * eased;
                        cx.notify();
                        t < 1.
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        }));
    }

    fn hover_media_card(&mut self, hovered: bool, cx: &mut Context<Self>) {
        let card = &mut self.browse_media;
        if card.hovered == hovered {
            return;
        }
        card.hovered = hovered;
        card.collapse = None;
        if hovered {
            self.animate_media_header(1., cx);
            return;
        }
        self.browse_media.collapse = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LINGER).await;
            let _ = this.update(cx, |this, cx| {
                if !this.browse_media.hovered {
                    this.animate_media_header(0., cx);
                }
            });
        }));
    }

    /// The media card at the foot of Browser mode's sidebar, while a tab
    /// off screen has media.
    pub(super) fn browse_media_card(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let candidates = self.browse_media_candidates(cx);
        self.browse_media.record(&candidates);
        let (pane, state) = self.browse_media_source(cx)?;
        let tab = self.browse_tab(pane, cx)?;
        let muted = self.browser_tab_sound(pane, cx) == Some(browser_audio::TabSound::Muted);
        let light = crate::theme::is_light();
        let ink = content(0.62);
        let hover = if light { black(0.075) } else { white(0.075) };
        let tip = |label: &'static str| {
            move |_: &mut Window, cx: &mut App| cx.new(|_| MediaTip(label.into())).into()
        };
        let favicon = match (tab.favicon.as_deref()).and_then(|url| crate::favicons::icon(url, cx))
        {
            Some(image) => gpui::img(image)
                .size(px(16.))
                .rounded(px(3.))
                .into_any_element(),
            None => icon("globe", 15.).text_color(ink).into_any_element(),
        };
        let playing = state.playing;
        let button = |name: &'static str,
                      label: &'static str,
                      enabled: bool,
                      glyph: AnyElement,
                      cx: &mut Context<Self>| {
            div()
                .flex_1()
                .h(px(ROW))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        // A new id when it turns: the old tooltip ("Pause")
                        // does not stay over the paused control.
                        .id((name, usize::from(name == "media-toggle" && playing)))
                        .debug_selector(move || name.into())
                        .role(Role::Button)
                        .aria_label(label)
                        .size(px(24.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(if enabled { ink } else { ink.opacity(0.32) })
                        .when(enabled, |d| d.hover(move |s| s.bg(hover)))
                        .tooltip(tip(label))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            let Some((current, state)) = this.browse_media_source(cx) else {
                                return;
                            };
                            if current != pane {
                                return;
                            }
                            let page = this.media_page(pane);
                            let act = |action: Action| {
                                if let Some(page) = &page {
                                    page.read(cx).media_action(action);
                                }
                            };
                            match name {
                                "media-return" => this.show_media_tab(pane, window, cx),
                                "media-previous" if state.previous => act(Action::Previous),
                                "media-next" if state.next => act(Action::Next),
                                "media-toggle" => act(Action::Toggle),
                                "media-mute" => this.set_browser_muted(pane, !muted, cx),
                                _ => {}
                            }
                        }))
                        .child(glyph),
                )
                .into_any_element()
        };
        let header = self.browse_media.header;
        let title: SharedString = if state.title.is_empty() {
            tab.title.clone().into()
        } else {
            state.title.clone().into()
        };
        let pip = div()
            .id("media-pip")
            .debug_selector(|| "media-pip".into())
            .role(Role::Button)
            .aria_label("Open Picture in Picture")
            .size(px(24.))
            .flex_none()
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .text_color(if state.video { ink } else { ink.opacity(0.32) })
            .when(state.video, |d| d.hover(move |s| s.bg(hover)))
            .tooltip(tip("Open Picture in Picture"))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if (this.browse_media_source(cx)).is_some_and(|(p, s)| p == pane && s.video)
                    && let Some(page) = this.media_page(pane)
                {
                    page.read(cx).open_picture_in_picture();
                }
            }))
            .child(pip_glyph());
        let dismiss = div()
            .id("media-dismiss")
            .debug_selector(|| "media-dismiss".into())
            .role(Role::Button)
            .aria_label("Close Mini Player")
            .size(px(24.))
            .flex_none()
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .text_color(ink)
            .hover(move |s| s.bg(hover))
            .tooltip(tip("Close Mini Player"))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                let card = &mut this.browse_media;
                card.dismissed.insert(pane);
                card.hovered = false;
                card.header = 0.;
                card.animation = None;
                card.collapse = None;
                cx.notify();
            }))
            .child(icon("close", 16.).text_color(ink));
        let title_row = div()
            .relative()
            .h(px(ROW * header))
            .flex_none()
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h(px(ROW))
                    .flex()
                    .items_center()
                    .pl(px(12.))
                    .pr(px(6.))
                    .gap(px(6.))
                    .child(
                        div()
                            .id("media-title")
                            .debug_selector(|| "media-title".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(0.85))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_media_tab(pane, window, cx)
                            }))
                            .child(title),
                    )
                    .child(pip)
                    .child(dismiss),
            );
        let controls = div()
            .flex()
            .h(px(ROW))
            .flex_none()
            .child(button("media-return", "Back to Tab", true, favicon, cx))
            .child(button(
                "media-previous",
                "Previous Track",
                state.previous,
                transport("previous"),
                cx,
            ))
            .child(button(
                "media-toggle",
                if state.playing { "Pause" } else { "Play" },
                true,
                transport(if state.playing { "pause" } else { "play" }),
                cx,
            ))
            .child(button(
                "media-next",
                "Next Track",
                state.next,
                transport("next"),
                cx,
            ))
            .child(button(
                "media-mute",
                if muted { "Unmute Tab" } else { "Mute Tab" },
                true,
                icon(if muted { "volume-off" } else { "volume-high" }, 16.)
                    .text_color(ink)
                    .into_any_element(),
                cx,
            ));
        let card = div()
            .id("media-card")
            .debug_selector(|| "media-card".into())
            .role(Role::Group)
            .aria_label("Media")
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .h(px(ROW * (1. + header)))
            .rounded(px(8.))
            .bg(if light { white(0.9) } else { white(0.07) })
            .when(light, |d| {
                d.shadow(vec![
                    BoxShadow::new(px(0.), px(1.), black(0.08)).blur_radius(px(3.)),
                ])
            })
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_hover(
                cx.listener(|this, hovered: &bool, _, cx| this.hover_media_card(*hovered, cx)),
            )
            .child(title_row)
            .child(controls);
        Some(
            div()
                .id("media-card-slot")
                .relative()
                .flex_none()
                .h(px(ROW))
                .mb(px(6.))
                .child(card)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn playing(title: &str) -> Playback {
        Playback {
            active: true,
            playing: true,
            video: true,
            sound: true,
            title: title.into(),
            next: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_card_follows_the_tab_that_started_playing_last() {
        let mut card = MediaCard::default();
        let a = (1, playing("A"));
        let b = (2, playing("B"));
        card.record(&[a.clone()]);
        assert_eq!(source(&card, &[a.clone()], &[]).map(|s| s.0), Some(1));
        // B starts: the card is B's.
        card.record(&[a.clone(), b.clone()]);
        assert_eq!(
            source(&card, &[a.clone(), b.clone()], &[]).map(|s| s.0),
            Some(2)
        );
        // On screen, B has no card: A's shows.
        assert_eq!(
            source(&card, &[a.clone(), b.clone()], &[2]).map(|s| s.0),
            Some(1)
        );
        // Dismissed, A's goes until it plays again.
        card.dismissed.insert(1);
        assert_eq!(source(&card, &[a.clone(), b.clone()], &[2]), None);
        let paused = (
            1,
            Playback {
                playing: false,
                ..playing("A")
            },
        );
        card.record(&[paused.clone(), b.clone()]);
        assert!(card.dismissed.contains(&1));
        card.record(&[a.clone(), b.clone()]);
        assert!(!card.dismissed.contains(&1));
        assert_eq!(card.pane, Some(1));
        // A page that never played has no card.
        let idle = (3, Playback::default());
        assert_eq!(source(&MediaCard::default(), &[idle], &[]), None);
    }

    #[gpui::test]
    fn a_tab_playing_out_of_sight_has_a_card_that_mutes_it_and_shows_it(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = super::super::tests::window(cx);
        let (first, second) = window
            .update(cx, |shell, window, cx| {
                shell.set_mode(crate::modes::AppMode::Browser, window, cx);
                let first = shell.browse_new_tab(
                    Some("https://music.test/".into()),
                    None,
                    true,
                    window,
                    cx,
                );
                let second =
                    shell.browse_new_tab(Some("https://b.test/".into()), None, true, window, cx);
                (first, second)
            })
            .unwrap();
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();
        let page = window
            .update(cx, |shell, _, cx| shell.browser_page(first, cx).unwrap())
            .unwrap();
        assert!(cx.debug_bounds("media-card").is_none());

        // The tab out of sight plays: its card shows, with its title.
        page.read_with(cx, |page, _| {
            page.set_playback_for_test(Some(playing("Song")))
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("media-card").is_some());
        assert!(cx.debug_bounds("media-toggle").is_some());

        // Its speaker mutes the tab.
        let mute = cx.debug_bounds("media-mute").expect("a mute button");
        cx.simulate_click(mute.center(), Default::default());
        cx.run_until_parked();
        let muted = window
            .update(cx, |shell, _, _| {
                shell.workspace.browser_panes[&first].muted
            })
            .unwrap();
        assert!(muted);

        // Back to Tab shows it, and the card goes with it on screen.
        let back = cx.debug_bounds("media-return").expect("Back to Tab");
        cx.simulate_click(back.center(), Default::default());
        cx.run_until_parked();
        let active = window
            .update(cx, |shell, _, _| shell.workspace.browse.active)
            .unwrap();
        assert_eq!(active, Some(first));
        assert!(cx.debug_bounds("media-card").is_none());

        // The other tab on screen again: the card comes back; its ✕ closes it.
        window
            .update(cx, |shell, window, cx| {
                shell.browse_select(second, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        assert!(cx.debug_bounds("media-card").is_some());
        window
            .update(cx, |shell, _, cx| shell.hover_media_card(true, cx))
            .unwrap();
        cx.run_until_parked();
        let close = cx.debug_bounds("media-dismiss").expect("a close button");
        cx.simulate_click(close.center(), Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("media-card").is_none());
    }
}
