//! Back and forward over the places the caret has been, across files: the
//! jumps made through symbol links, and the caret's own moves to another
//! file or to a far part of one. It remembers places, not editors, so going
//! back to a closed file opens it.
use std::time::{Duration, Instant};

use super::Location;

const LIMIT: usize = 50;
/// Caret moves shorter than this many lines stay on the place they started
/// from, which follows the caret, so typing and nudging leave no trail.
const NEAR: usize = 10;
/// How long a Back or Forward waits for its editor to land on the place.
const ARRIVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct History {
    entries: Vec<Location>,
    /// The entry the user is at.
    index: usize,
    /// The place a Back or Forward is opening. Until the caret gets there,
    /// what the opening editor reports on the way (the top of a file that
    /// is still loading) is not a move of the user's.
    arriving: Option<(Location, Instant)>,
}
fn near(a: &Location, b: &Location) -> bool {
    a.path == b.path && a.line.abs_diff(b.line) < NEAR
}
impl History {
    /// Records the caret at `place`. Into another file or far within one,
    /// it leaves the place it was on behind, so Back returns there; a short
    /// move only carries the current place along. Returns whether the trail
    /// changed.
    pub(crate) fn visit(&mut self, place: Location) -> bool {
        if let Some((target, since)) = &self.arriving {
            if since.elapsed() < ARRIVAL && !near(target, &place) {
                return false;
            }
            self.arriving = None;
        }
        let Some(here) = self.entries.get_mut(self.index) else {
            self.entries.push(place);
            self.index = 0;
            return true;
        };
        if near(here, &place) {
            *here = place;
            return false;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(place);
        let extra = self.entries.len().saturating_sub(LIMIT);
        self.entries.drain(..extra);
        self.index = self.entries.len() - 1;
        true
    }
    /// Records leaving `from` for `to`. Whatever lay forward is dropped.
    pub(crate) fn jump(&mut self, from: Location, to: Location) {
        self.entries.truncate(self.index + 1);
        match self.entries.last_mut() {
            // Jumping again from where the last jump landed extends the
            // trail instead of recording the same line twice.
            Some(last) if last.path == from.path && last.line == from.line => *last = from,
            _ => self.entries.push(from),
        }
        self.entries.push(to);
        let extra = self.entries.len().saturating_sub(LIMIT);
        self.entries.drain(..extra);
        self.index = self.entries.len() - 1;
    }
    /// The place before the current one. `current` is where the caret is
    /// now, so coming forward again returns to it and not to where the jump
    /// first landed.
    pub(crate) fn back(&mut self, current: Option<Location>) -> Option<Location> {
        if !self.can_back() {
            return None;
        }
        if let Some(current) = current {
            self.entries[self.index] = current;
        }
        self.index -= 1;
        Some(self.arrive())
    }
    pub(crate) fn forward(&mut self, current: Option<Location>) -> Option<Location> {
        if !self.can_forward() {
            return None;
        }
        if let Some(current) = current {
            self.entries[self.index] = current;
        }
        self.index += 1;
        Some(self.arrive())
    }
    fn arrive(&mut self) -> Location {
        let to = self.entries[self.index].clone();
        self.arriving = Some((to.clone(), Instant::now()));
        to
    }
    pub(crate) fn can_back(&self) -> bool {
        self.index > 0
    }
    pub(crate) fn can_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }
    /// Whether there is anywhere to go, for the header's arrows.
    pub(crate) fn is_walkable(&self) -> bool {
        self.can_back() || self.can_forward()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: &str, line: usize) -> Location {
        Location {
            path: path.into(),
            line,
            column: 1,
        }
    }

    #[test]
    fn back_and_forward_walk_the_trail_and_a_new_jump_cuts_it() {
        let mut history = History::default();
        assert!(history.back(None).is_none());
        history.jump(at("/a.py", 10), at("/b.py", 3));
        history.jump(at("/b.py", 3), at("/c.py", 7));
        assert_eq!(history.entries.len(), 3, "a landing is not recorded twice");
        // The caret moved to line 9 of c.py before going back.
        assert_eq!(history.back(Some(at("/c.py", 9))), Some(at("/b.py", 3)));
        assert_eq!(history.back(None), Some(at("/a.py", 10)));
        assert!(!history.can_back());
        assert_eq!(history.forward(None), Some(at("/b.py", 3)));
        assert_eq!(history.forward(None), Some(at("/c.py", 9)));
        assert!(!history.can_forward());
        history.back(None);
        history.jump(at("/b.py", 5), at("/d.py", 1));
        assert!(!history.can_forward());
        assert_eq!(history.back(None), Some(at("/b.py", 5)));
    }

    #[test]
    fn far_moves_and_other_files_leave_a_step_and_short_ones_do_not() {
        let mut history = History::default();
        assert!(history.visit(at("/a.py", 10)));
        // Typing and nudging carry the place along.
        assert!(!history.visit(at("/a.py", 12)));
        assert!(!history.visit(at("/a.py", 19)));
        assert!(!history.can_back());
        // A click far down the file leaves line 19 behind.
        assert!(history.visit(at("/a.py", 200)));
        // Opening another file leaves line 200 behind.
        assert!(history.visit(at("/b.py", 5)));
        assert_eq!(history.back(None), Some(at("/a.py", 200)));
        assert_eq!(history.back(None), Some(at("/a.py", 19)));
        assert!(!history.can_back());
        assert_eq!(history.forward(None), Some(at("/a.py", 200)));
        assert_eq!(history.forward(None), Some(at("/b.py", 5)));
        assert!(!history.can_forward());
    }

    #[test]
    fn a_landing_from_back_is_not_a_new_move() {
        let mut history = History::default();
        history.visit(at("/a.py", 100));
        history.visit(at("/b.py", 1));
        assert_eq!(history.back(Some(at("/b.py", 3))), Some(at("/a.py", 100)));
        // The editor opening a.py shows its top before it reaches line 100.
        assert!(!history.visit(at("/a.py", 1)));
        assert!(!history.visit(at("/a.py", 100)));
        assert!(history.can_forward(), "forward survives the landing");
        // Once there, the user's own moves count again and cut the trail.
        assert!(history.visit(at("/c.py", 1)));
        assert!(!history.can_forward());
        assert_eq!(history.back(None), Some(at("/a.py", 100)));
    }

    #[test]
    fn a_symbol_jump_and_its_landing_make_one_step() {
        let mut history = History::default();
        history.visit(at("/a.py", 10));
        history.jump(at("/a.py", 10), at("/b.py", 40));
        assert!(!history.visit(at("/b.py", 40)));
        assert_eq!(history.back(None), Some(at("/a.py", 10)));
        assert!(!history.can_back());
    }

    #[test]
    fn the_trail_is_bounded() {
        let mut history = History::default();
        for n in 0..200 {
            history.jump(at("/a.py", n * 2), at("/a.py", n * 2 + 1));
        }
        assert_eq!(history.entries.len(), LIMIT);
        assert_eq!(history.back(None), Some(at("/a.py", 398)));
    }
}
