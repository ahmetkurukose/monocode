//! Who drives a page, the user or an agent (context.md K47, K48).
//!
//! An agent counts as active on a page until 20 s after its last command
//! there; the app shows its badge and label for that long. While it is
//! active, a click or a key of the user's on the page takes control: the
//! agent's input tools answer `browser_user_control` until the user gives
//! control back, or stops touching the page for 16 s. Scrolling and moving
//! the mouse never take control, and touching a page no agent is using
//! takes nothing.
//!
//! Pure bookkeeping: the app feeds it the user's input and the tools'
//! commands, with the time, and asks it.

use std::time::{Duration, Instant};

/// How long after its last command an agent counts as using a page.
pub const AGENT_ACTIVE: Duration = Duration::from_secs(20);
/// How long the user keeps control after their last input.
pub const USER_HOLD: Duration = Duration::from_secs(16);

/// What the user did on a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserInput {
    Click,
    Key,
    Scroll,
    MouseMove,
}

/// The user drives the page; the agent's input waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserControl {
    /// When control returns to the agent if the user stops now.
    pub resumes_in: Duration,
}

#[derive(Clone, Debug, Default)]
pub struct ControlState {
    last_agent: Option<Instant>,
    /// The user's last click or key while they hold control.
    user_last: Option<Instant>,
}

impl ControlState {
    /// An agent sent the page a command.
    pub fn agent_used(&mut self, now: Instant) {
        self.last_agent = Some(now);
    }

    /// Whether an agent counts as using the page (its badge shows).
    pub fn agent_active(&self, now: Instant) -> bool {
        self.last_agent
            .is_some_and(|at| now.saturating_duration_since(at) < AGENT_ACTIVE)
    }

    /// The user gave the page input.
    pub fn user_input(&mut self, input: UserInput, now: Instant) {
        if !matches!(input, UserInput::Click | UserInput::Key) {
            return;
        }
        // Holding control already: every click or key extends it.
        if self.user_control(now).is_some() || self.agent_active(now) {
            self.user_last = Some(now);
        }
    }

    /// The user gave control back (the toolbar's button).
    pub fn give_back(&mut self) {
        self.user_last = None;
    }

    /// Whether the user holds control now, and for how much longer.
    pub fn user_control(&self, now: Instant) -> Option<UserControl> {
        let held = now.saturating_duration_since(self.user_last?);
        (held < USER_HOLD).then(|| UserControl {
            resumes_in: USER_HOLD - held,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn a_click_while_an_agent_works_takes_control_for_16_s() {
        let t = Instant::now();
        let mut c = ControlState::default();
        c.agent_used(t);
        c.user_input(UserInput::Click, t + S);
        assert_eq!(
            c.user_control(t + 2 * S),
            Some(UserControl { resumes_in: 15 * S })
        );
        // More keys keep it.
        c.user_input(UserInput::Key, t + 10 * S);
        assert!(c.user_control(t + 25 * S).is_some());
        assert_eq!(c.user_control(t + 26 * S), None);
    }

    #[test]
    fn scrolling_and_moving_never_take_control() {
        let t = Instant::now();
        let mut c = ControlState::default();
        c.agent_used(t);
        c.user_input(UserInput::Scroll, t + S);
        c.user_input(UserInput::MouseMove, t + S);
        assert_eq!(c.user_control(t + 2 * S), None);
    }

    #[test]
    fn touching_a_page_no_agent_uses_takes_nothing() {
        let t = Instant::now();
        let mut c = ControlState::default();
        c.user_input(UserInput::Click, t);
        assert_eq!(c.user_control(t), None);
        // An agent idle for 20 s no longer counts.
        c.agent_used(t);
        assert!(c.agent_active(t + 19 * S));
        c.user_input(UserInput::Click, t + 20 * S);
        assert_eq!(c.user_control(t + 20 * S), None);
    }

    #[test]
    fn giving_control_back_ends_it_at_once() {
        let t = Instant::now();
        let mut c = ControlState::default();
        c.agent_used(t);
        c.user_input(UserInput::Key, t);
        c.give_back();
        assert_eq!(c.user_control(t + S), None);
    }
}
