//! Where the documentation box of the selected completion goes: beside the
//! list where there is room, else above or below it, and never over the list
//! or the caret's line. Pure geometry, so it is tested without a window.

/// A rectangle in the editor's own coordinates (the gutter included).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Area {
    pub(crate) fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub(crate) fn right(&self) -> f32 {
        self.x + self.w
    }
    pub(crate) fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub(crate) fn overlaps(&self, other: &Area) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// The widest and tallest the box grows.
const MAX_W: f32 = 320.;
const MAX_H: f32 = 240.;
/// Below these it is not worth showing: a few words in a sliver.
const MIN_W: f32 = 140.;
const MIN_H: f32 = 48.;
/// The gap to the list and to the editor's edge.
const GAP: f32 = 4.;

/// The box for `container` (the editor's width and height), given the
/// completion `list` and the caret's `line` (the full width of its row).
/// `None` when there is no room that leaves both uncovered.
pub(crate) fn documentation(container: (f32, f32), list: Area, line: Area) -> Option<Area> {
    let (width, height) = container;
    let below_line = list.y >= line.y;
    // Beside the list, the box shares its rows but stops at the caret's line:
    // from the list's top down when the list is under the line, up from the
    // list's bottom when it is over it.
    let (from, to) = match below_line {
        true => (list.y, height - GAP),
        false => (GAP, list.bottom().min(line.y)),
    };
    let side_h = MAX_H.min(to - from);
    let side_y = if below_line { from } else { to - side_h };
    let beside =
        |x: f32, w: f32| (w >= MIN_W && side_h >= MIN_H).then(|| Area::new(x, side_y, w, side_h));
    let right_x = list.right() + GAP;
    let right = beside(right_x, MAX_W.min(width - GAP - right_x));
    let left_w = MAX_W.min(list.x - GAP - GAP);
    let left = beside(list.x - GAP - left_w, left_w);

    // Across the list's width, in the free band on one side of it.
    let across_w = MAX_W.min(width - 2. * GAP);
    let across_x = list.x.clamp(GAP, (width - GAP - across_w).max(GAP));
    let band = |from: f32, to: f32, top_down: bool| {
        let h = MAX_H.min(to - from);
        (across_w >= MIN_W && h >= MIN_H).then(|| {
            let y = if top_down { from } else { to - h };
            Area::new(across_x, y, across_w, h)
        })
    };
    // Past the list on its far side, then past the caret's line on the other.
    let under = |top: f32| band(top + GAP, height - GAP, true);
    let over = |bottom: f32| band(GAP, bottom - GAP, false);
    let (near, far) = match below_line {
        true => (under(list.bottom()), over(line.y)),
        false => (over(list.y), under(line.bottom())),
    };
    [right, left, near, far]
        .into_iter()
        .flatten()
        .find(|area| !area.overlaps(&list) && !area.overlaps(&line))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: f32 = 20.;

    /// A list of `w` by `h` under (or over) a caret line at `y`.
    fn setup(x: f32, y: f32, w: f32, h: f32, above: bool) -> (Area, Area) {
        let line = Area::new(0., y, 1000., LINE);
        let top = if above { y - h } else { y + LINE };
        (Area::new(x, top, w, h), line)
    }

    fn clear(area: Area, container: (f32, f32), list: Area, line: Area) {
        assert!(!area.overlaps(&list), "{area:?} covers the list {list:?}");
        assert!(!area.overlaps(&line), "{area:?} covers the line {line:?}");
        assert!(area.x >= 0. && area.y >= 0., "{area:?} leaves the editor");
        assert!(area.right() <= container.0 && area.bottom() <= container.1);
    }

    #[test]
    fn goes_to_the_right_of_the_list_when_there_is_room() {
        let container = (900., 600.);
        let (list, line) = setup(100., 100., 220., 230., false);
        let area = documentation(container, list, line).unwrap();
        assert_eq!((area.x, area.y), (list.right() + GAP, list.y));
        assert_eq!(area.w, MAX_W);
        clear(area, container, list, line);
    }

    #[test]
    fn goes_to_the_left_when_only_the_left_has_room() {
        let container = (520., 600.);
        let (list, line) = setup(300., 100., 200., 100., false);
        let area = documentation(container, list, line).unwrap();
        assert_eq!(area.right(), list.x - GAP);
        assert!(area.w >= MIN_W);
        clear(area, container, list, line);
    }

    #[test]
    fn a_narrow_pane_puts_it_below_or_above_the_list() {
        let container = (260., 600.);
        // The list fills the width: neither side has room.
        let (list, line) = setup(20., 100., 230., 100., false);
        let area = documentation(container, list, line).unwrap();
        assert!(area.y >= list.bottom());
        assert!(area.w <= container.0 - 2. * GAP);
        clear(area, container, list, line);

        // Near the bottom the list is above the line, and so is the box.
        let (list, line) = setup(20., 400., 230., 150., true);
        let container = (260., 450.);
        let area = documentation(container, list, line).unwrap();
        assert!(area.bottom() <= list.y);
        clear(area, container, list, line);
    }

    #[test]
    fn beside_a_list_over_the_line_it_stops_at_the_line() {
        let container = (900., 440.);
        let (list, line) = setup(100., 400., 220., 120., true);
        let area = documentation(container, list, line).unwrap();
        assert!(area.x >= list.right());
        assert!(area.bottom() <= line.y);
        clear(area, container, list, line);
    }

    #[test]
    fn nothing_is_shown_when_there_is_no_room() {
        // A pane the list fills in both directions.
        let container = (240., 130.);
        let (list, line) = setup(4., 4., 232., 100., false);
        assert_eq!(documentation(container, list, line), None);
    }

    #[test]
    fn never_covers_the_list_or_the_line_anywhere() {
        for width in [150., 260., 400., 700., 1200.] {
            for height in [120., 240., 400., 800.] {
                for caret_y in (0..height as i32).step_by(37) {
                    for caret_x in (0..width as i32).step_by(53) {
                        let list_w = 180_f32.min(width - 8.);
                        let list_h = 120_f32.min(height - LINE - 8.);
                        if list_h < 20. {
                            continue;
                        }
                        let y = caret_y as f32;
                        let above = y + LINE + list_h > height;
                        let top = if above {
                            (y - list_h).max(0.)
                        } else {
                            y + LINE
                        };
                        let x = (caret_x as f32).clamp(4., (width - list_w - 4.).max(4.));
                        let list = Area::new(x, top, list_w, list_h);
                        let line = Area::new(0., y, width, LINE);
                        if let Some(area) = documentation((width, height), list, line) {
                            clear(area, (width, height), list, line);
                        }
                    }
                }
            }
        }
    }
}
