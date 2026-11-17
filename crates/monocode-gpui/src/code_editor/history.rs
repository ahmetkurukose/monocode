//! Back and forward over the jumps made through symbol links, across files.
//! It remembers places, not editors, so going back to a closed file opens it.
use super::Location;

const LIMIT: usize = 50;

#[derive(Default)]
pub(crate) struct History {
    entries: Vec<Location>,
    /// The entry the user is at.
    index: usize,
}
impl History {
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
        Some(self.entries[self.index].clone())
    }
    pub(crate) fn forward(&mut self, current: Option<Location>) -> Option<Location> {
        if !self.can_forward() {
            return None;
        }
        if let Some(current) = current {
            self.entries[self.index] = current;
        }
        self.index += 1;
        Some(self.entries[self.index].clone())
    }
    pub(crate) fn can_back(&self) -> bool {
        self.index > 0
    }
    pub(crate) fn can_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
    fn the_trail_is_bounded() {
        let mut history = History::default();
        for n in 0..200 {
            history.jump(at("/a.py", n * 2), at("/a.py", n * 2 + 1));
        }
        assert_eq!(history.entries.len(), LIMIT);
        assert_eq!(history.back(None), Some(at("/a.py", 398)));
    }
}
