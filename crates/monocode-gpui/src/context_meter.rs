use crate::theme::*;
use gpui::{
    Animation, AnimationExt, AnyElement, BoxShadow, Hsla, div, hsla, linear_color_stop,
    linear_gradient, point, prelude::*, px,
};
use std::time::Duration;

/// Past this the bar starts blushing toward red.
pub(crate) const WARM: f32 = 0.5;
/// Past this the thread is close to full: the bar glows and a skull shows.
pub(crate) const CRITICAL: f32 = 0.8;

const TRACK_WIDTH: f32 = 64.;
const TRACK_HEIGHT: f32 = 6.;

fn red(alpha: f32) -> Hsla {
    hsla(0., 0.84, 0.6, alpha)
}

/// The meter's ink: quiet grey up to `WARM`, then a steady blend into red
/// that is fully red when the window is full.
pub(crate) fn tint(ratio: f32) -> Hsla {
    let base = content(0.55);
    let t = ((ratio - WARM) / (1. - WARM)).clamp(0., 1.).powf(0.8);
    if t == 0. {
        return base;
    }
    let to = red(1.);
    hsla(
        to.h,
        base.s + (to.s - base.s) * t,
        base.l + (to.l - base.l) * t,
        base.a + (to.a - base.a) * t,
    )
}

/// A pill holding a slim progress bar and the percentage. Notches mark 50%
/// and 80%; past 80% the fill glows and a pulsing skull leads the pill.
pub(crate) fn bar(id: &'static str, ratio: f32) -> AnyElement {
    let fill = ratio.clamp(0., 1.);
    let ink = tint(ratio);
    let critical = ratio >= CRITICAL;
    let warm = ratio >= WARM;
    let notch = |at: f32| {
        div()
            .absolute()
            .top_0()
            .left(px(TRACK_WIDTH * at - 0.5))
            .w(px(1.))
            .h_full()
            .bg(content(0.22))
    };
    let track = div()
        .relative()
        .flex_shrink_0()
        .w(px(TRACK_WIDTH))
        .h(px(TRACK_HEIGHT))
        .rounded_full()
        .bg(content(0.1))
        .child(notch(WARM))
        .child(notch(CRITICAL))
        .when(fill > 0., |d| {
            let start = if warm { tint(WARM) } else { content(0.4) };
            d.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h_full()
                    .w(px((TRACK_WIDTH * fill).max(TRACK_HEIGHT)))
                    .rounded_full()
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(start, 0.),
                        linear_color_stop(ink, 1.),
                    ))
                    .when(warm, |d| {
                        let glow = if critical { 0.55 } else { 0.3 };
                        d.shadow(vec![BoxShadow {
                            color: red(glow * ((ratio - WARM) / (1. - WARM)).clamp(0.3, 1.)),
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(if critical { 8. } else { 5. }),
                            spread_radius: px(0.),
                            inset: false,
                        }])
                    }),
            )
        });
    div()
        .flex_shrink_0()
        .h(px(22.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded_full()
        .border_1()
        .border_color(if critical { red(0.35) } else { content(0.08) })
        .bg(if critical { red(0.1) } else { content(0.03) })
        .when(critical, |d| {
            d.child(
                div()
                    .text_size(px(12.))
                    .line_height(px(14.))
                    .child("💀")
                    .with_animation(
                        id,
                        Animation::new(Duration::from_millis(1600))
                            .repeat()
                            .with_easing(gpui::pulsating_between(0.45, 1.)),
                        |el, t| el.opacity(t),
                    ),
            )
        })
        .child(track)
        .child(
            div()
                .min_w(px(28.))
                .text_size(px(11.))
                .line_height(px(14.))
                .font_weight(if warm {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::MEDIUM
                })
                .text_color(ink)
                .text_right()
                .child(format!("{:.0}%", ratio * 100.)),
        )
        .into_any_element()
}

/// One line under the headline that says what the level means.
pub(crate) fn advice(ratio: f32) -> Option<&'static str> {
    if ratio >= CRITICAL {
        Some("Nearly full — older turns will be compacted soon.")
    } else if ratio >= WARM {
        Some("Over half full.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_stays_grey_until_half_then_reddens() {
        assert_eq!(tint(0.2), tint(0.5));
        assert!(tint(0.7).s > tint(0.5).s);
        assert!(tint(0.9).s > tint(0.7).s);
        assert_eq!(tint(1.), red(1.));
    }
}
