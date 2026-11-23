//! What a browser page out of sight may lose (context.md K64, K52): one
//! set of leases decides whether a page stays live, and while none holds
//! it, how long it has been idle.
//!
//! - **Memory Saver.** A page idle for the setting's time (30 min by
//!   default) is discarded: its Chromium browser closes, its tab keeps the
//!   address and title, and it loads again when shown or used by an agent
//!   (the K20 lazy path).
//! - **Battery Saver.** On a low battery a page idle for a minute is frozen
//!   (`Page.setWebLifecycleState`): no timers, no scripts. It thaws the
//!   moment anything leases it again. The page the user sees and the ones
//!   an agent drives keep their full frame rate; pages out of sight draw
//!   no frames at all (their views are hidden), so this is the only
//!   throttling left to do for them.
//!
//! Leases are what the app knows at once ([`Signals`]) and what only the
//! page can say ([`Probe`]: playing media, an edited form, a `beforeunload`
//! handler), asked just before a page would be discarded or frozen.
//!
//! Pure bookkeeping: the page feeds it the time and its state.

use std::time::{Duration, Instant};

/// Why a page must stay live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lease {
    /// Its view is on screen, under an overlay, or drawn out of the window
    /// for an agent (K23).
    Shown,
    /// An agent's command runs on it, or an external CDP client is attached.
    Command,
    /// An agent used it in the last 20 s (K47).
    AgentRecent,
    DevTools,
    /// A capture of its frame is on its way.
    Capture,
    Download,
    /// A JavaScript dialog or sign-in waits on the user.
    Dialog,
    /// Its tab is pinned in its group.
    Pinned,
    /// A video or audio element plays with sound.
    Audio,
    /// The user typed into a form that was not submitted since.
    EditedForm,
    /// The page asks before it is left (`beforeunload`).
    BeforeUnload,
    /// The page did not say (no answer yet, or not in time).
    Unknown,
}

impl Lease {
    const ALL: [Lease; 12] = [
        Lease::Shown,
        Lease::Command,
        Lease::AgentRecent,
        Lease::DevTools,
        Lease::Capture,
        Lease::Download,
        Lease::Dialog,
        Lease::Pinned,
        Lease::Audio,
        Lease::EditedForm,
        Lease::BeforeUnload,
        Lease::Unknown,
    ];

    fn bit(self) -> u16 {
        1 << (self as u16)
    }

    /// Whether a frozen page would still lose something: its sound, or
    /// whatever the page could not tell. A form survives freezing.
    fn holds_awake(self) -> bool {
        !matches!(self, Lease::EditedForm | Lease::BeforeUnload)
    }
}

/// A set of [`Lease`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Leases(u16);

impl Leases {
    pub fn with(mut self, lease: Lease, held: bool) -> Self {
        if held {
            self.0 |= lease.bit();
        }
        self
    }

    pub fn has(self, lease: Lease) -> bool {
        self.0 & lease.bit() != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[cfg(test)]
    fn union(self, other: Leases) -> Leases {
        Leases(self.0 | other.0)
    }

    fn holds_awake(self) -> bool {
        self.iter().any(Lease::holds_awake)
    }

    pub fn iter(self) -> impl Iterator<Item = Lease> {
        Lease::ALL.into_iter().filter(move |lease| self.has(*lease))
    }
}

/// What the app knows about a page without asking it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Signals {
    pub shown: bool,
    pub commands: bool,
    pub agent_recent: bool,
    pub devtools: bool,
    pub capture: bool,
    pub download: bool,
    pub dialog: bool,
    pub pinned: bool,
}

impl Signals {
    pub fn leases(&self) -> Leases {
        Leases::default()
            .with(Lease::Shown, self.shown)
            .with(Lease::Command, self.commands)
            .with(Lease::AgentRecent, self.agent_recent)
            .with(Lease::DevTools, self.devtools)
            .with(Lease::Capture, self.capture)
            .with(Lease::Download, self.download)
            .with(Lease::Dialog, self.dialog)
            .with(Lease::Pinned, self.pinned)
    }
}

/// What the page said when asked, and when.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Probe {
    pub at: Instant,
    /// `None`: the page did not answer.
    pub answer: Option<ProbeAnswer>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ProbeAnswer {
    pub audible: bool,
    pub edited: bool,
    pub before_unload: bool,
    /// The edit watcher was in the document (it is added for each new one).
    pub watched: bool,
    /// Where the page was scrolled to.
    pub scroll: (f64, f64),
}

impl Probe {
    pub fn leases(&self) -> Leases {
        let Some(answer) = self.answer else {
            return Leases::default().with(Lease::Unknown, true);
        };
        Leases::default()
            .with(Lease::Audio, answer.audible)
            .with(Lease::EditedForm, answer.edited)
            .with(Lease::BeforeUnload, answer.before_unload)
            .with(Lease::Unknown, !answer.watched)
    }
}

/// An answer this old is asked again.
pub(crate) const PROBE_FRESH: Duration = Duration::from_secs(60);
/// A page idle this long on a low battery freezes.
pub(crate) const FREEZE_AFTER: Duration = Duration::from_secs(60);
/// Memory Saver's default.
pub(crate) const DEFAULT_DISCARD_MINUTES: u32 = 30;

/// The settings and the battery, for one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Policy {
    /// Memory Saver: idle this long, a page is discarded. `None`: off.
    pub discard_after: Option<Duration>,
    /// Battery Saver applies now (it is on and the battery is low).
    pub freeze: bool,
    pub freeze_after: Duration,
}

impl Policy {
    /// From the settings: Memory Saver's minutes (0 is off), Battery
    /// Saver's switch, and whether the battery is low. `discard_secs`
    /// (the development hook) replaces the minutes.
    pub fn new(
        memory_saver_minutes: u32,
        discard_secs: Option<u64>,
        battery_saver: bool,
        battery_low: bool,
    ) -> Policy {
        let discard_after = match discard_secs {
            Some(secs) => Some(Duration::from_secs(secs.max(1))),
            None if memory_saver_minutes == 0 => None,
            None => Some(Duration::from_secs(u64::from(memory_saver_minutes) * 60)),
        };
        Policy {
            discard_after,
            freeze: battery_saver && battery_low,
            freeze_after: FREEZE_AFTER,
        }
    }

    /// How often pages are looked at: a tenth of the shortest wait, between
    /// 1 and 30 seconds.
    pub fn tick(&self) -> Duration {
        let shortest = [self.discard_after, self.freeze.then_some(self.freeze_after)]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(Duration::from_secs(300));
        (shortest / 10).clamp(Duration::from_secs(1), Duration::from_secs(30))
    }
}

/// What to do with a page now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Stay,
    /// Ask the page what it holds (playing media, an edited form) first.
    Probe,
    Discard,
    Freeze,
    Thaw,
}

/// A page's lifecycle bookkeeping.
#[derive(Clone, Debug, Default)]
pub(crate) struct Life {
    /// Since when no lease held the page.
    idle_since: Option<Instant>,
    probe: Option<Probe>,
    /// A probe is on its way.
    pub probing: bool,
}

impl Life {
    /// The page answered (or did not, `None`).
    pub fn probed(&mut self, answer: Option<ProbeAnswer>, now: Instant) {
        self.probing = false;
        self.probe = Some(Probe { at: now, answer });
    }

    /// The last answer, if any.
    pub fn last_probe(&self) -> Option<&Probe> {
        self.probe.as_ref()
    }

    /// What becomes of the page at `now`, held by `leases`, `frozen` or not.
    pub fn tick(&mut self, leases: Leases, frozen: bool, policy: &Policy, now: Instant) -> Step {
        if !leases.is_empty() {
            self.idle_since = None;
            // A page in use again says anew what it holds.
            self.probe = None;
            return if frozen { Step::Thaw } else { Step::Stay };
        }
        let idle = now.saturating_duration_since(*self.idle_since.get_or_insert(now));
        // A frozen page runs nothing: what it said before still holds.
        let probe = (self.probe)
            .filter(|p| frozen || now.saturating_duration_since(p.at) < PROBE_FRESH)
            .map(|p| p.leases());
        let ask = if self.probing {
            Step::Stay
        } else {
            Step::Probe
        };
        if policy.discard_after.is_some_and(|after| idle >= after) {
            match probe {
                None => return ask,
                Some(held) if held.is_empty() => return Step::Discard,
                Some(_) => {}
            }
        }
        if policy.freeze && !frozen && idle >= policy.freeze_after {
            return match probe {
                None => ask,
                Some(held) if !held.holds_awake() => Step::Freeze,
                Some(_) => Step::Stay,
            };
        }
        if frozen && !policy.freeze {
            return Step::Thaw;
        }
        Step::Stay
    }

    /// Why the page is kept: `signals` and what it said when last asked.
    #[cfg(test)]
    pub fn held(&self, signals: Leases) -> Leases {
        signals.union(self.probe.map(|p| p.leases()).unwrap_or_default())
    }
}

/// Whether the Mac's battery is low: below Apple's early warning (22%)
/// while on battery, or Low Power Mode is on. False when it cannot tell.
/// `MONOCODE_BROWSER_BATTERY_LOW=1` says yes (development hook).
pub(crate) fn battery_low() -> bool {
    if std::env::var("MONOCODE_BROWSER_BATTERY_LOW").as_deref() == Ok("1") {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        mac::battery_low()
    }
    #[cfg(not(target_os = "macos"))]
    false
}

#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        /// `kIOPSLowBatteryWarningNone` (1), `…Early` (2), `…Final` (3).
        fn IOPSGetBatteryWarningLevel() -> i32;
    }

    pub(super) fn battery_low() -> bool {
        // SAFETY: no arguments; reads the power sources' state.
        let warning = unsafe { IOPSGetBatteryWarningLevel() };
        warning > 1 || low_power_mode()
    }

    fn low_power_mode() -> bool {
        use objc2::{msg_send, runtime::AnyObject};
        let Some(class) = objc2::runtime::AnyClass::get(c"NSProcessInfo") else {
            return false;
        };
        // SAFETY: `processInfo` is a class method returning the shared
        // instance; `isLowPowerModeEnabled` (macOS 12) a BOOL property,
        // asked only where it exists.
        unsafe {
            let info: *mut AnyObject = msg_send![class, processInfo];
            let Some(info) = info.as_ref() else {
                return false;
            };
            let asks: bool =
                msg_send![info, respondsToSelector: objc2::sel!(isLowPowerModeEnabled)];
            asks && msg_send![info, isLowPowerModeEnabled]
        }
    }
}

/// `MONOCODE_BROWSER_DISCARD_AFTER_SECS` (development hook): Memory Saver
/// discards after this many seconds, whatever the setting says.
pub(crate) fn discard_secs_override() -> Option<u64> {
    std::env::var("MONOCODE_BROWSER_DISCARD_AFTER_SECS")
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn policy(discard_secs: u64, freeze: bool) -> Policy {
        Policy {
            discard_after: Some(Duration::from_secs(discard_secs)),
            freeze,
            freeze_after: 10 * S,
        }
    }

    fn clean() -> Option<ProbeAnswer> {
        Some(ProbeAnswer {
            watched: true,
            ..Default::default()
        })
    }

    #[test]
    fn signals_and_answers_are_leases() {
        let signals = Signals {
            shown: true,
            pinned: true,
            ..Default::default()
        };
        let leases = signals.leases();
        assert!(leases.has(Lease::Shown) && leases.has(Lease::Pinned));
        assert!(!leases.has(Lease::Command));
        assert_eq!(
            leases.iter().collect::<Vec<_>>(),
            [Lease::Shown, Lease::Pinned]
        );
        assert!(Signals::default().leases().is_empty());

        let t = Instant::now();
        let silent = Probe {
            at: t,
            answer: None,
        };
        assert!(silent.leases().has(Lease::Unknown));
        let unwatched = Probe {
            at: t,
            answer: Some(ProbeAnswer::default()),
        };
        assert!(unwatched.leases().has(Lease::Unknown));
        let edited = Probe {
            at: t,
            answer: Some(ProbeAnswer {
                watched: true,
                edited: true,
                ..Default::default()
            }),
        };
        assert_eq!(
            edited.leases().iter().collect::<Vec<_>>(),
            [Lease::EditedForm]
        );
    }

    #[test]
    fn the_setting_and_the_hook_make_the_policy() {
        let p = Policy::new(30, None, true, false);
        assert_eq!(p.discard_after, Some(1800 * S));
        assert!(!p.freeze);
        assert_eq!(p.tick(), 30 * S);
        assert_eq!(Policy::new(0, None, true, false).discard_after, None);
        // The hook wins over the setting, even off.
        let hooked = Policy::new(0, Some(5), true, false);
        assert_eq!(hooked.discard_after, Some(5 * S));
        assert_eq!(hooked.tick(), S);
        // Battery Saver only applies on a low battery, and only when on.
        assert!(Policy::new(30, None, true, true).freeze);
        assert!(!Policy::new(30, None, false, true).freeze);
        assert_eq!(Policy::new(30, None, true, true).tick(), 6 * S);
        // Nothing to do: a slow look now and then.
        assert_eq!(Policy::new(0, None, false, false).tick(), 30 * S);
    }

    #[test]
    fn an_idle_page_is_asked_then_discarded() {
        let t = Instant::now();
        let p = policy(100, false);
        let mut life = Life::default();
        let none = Leases::default();
        assert_eq!(life.tick(none, false, &p, t), Step::Stay);
        assert_eq!(life.tick(none, false, &p, t + 99 * S), Step::Stay);
        assert_eq!(life.tick(none, false, &p, t + 100 * S), Step::Probe);
        life.probing = true;
        // Not asked twice while the answer is on its way.
        assert_eq!(life.tick(none, false, &p, t + 101 * S), Step::Stay);
        life.probed(clean(), t + 101 * S);
        assert_eq!(life.tick(none, false, &p, t + 102 * S), Step::Discard);
    }

    #[test]
    fn any_lease_restarts_the_idle_time() {
        let t = Instant::now();
        let p = policy(100, false);
        let mut life = Life::default();
        let none = Leases::default();
        life.tick(none, false, &p, t);
        for lease in [
            Lease::Shown,
            Lease::AgentRecent,
            Lease::Command,
            Lease::DevTools,
        ] {
            let held = Leases::default().with(lease, true);
            assert_eq!(
                life.tick(held, false, &p, t + 200 * S),
                Step::Stay,
                "{lease:?}"
            );
        }
        // Idle again from here: the 100 s start over.
        assert_eq!(life.tick(none, false, &p, t + 201 * S), Step::Stay);
        assert_eq!(life.tick(none, false, &p, t + 300 * S), Step::Stay);
        assert_eq!(life.tick(none, false, &p, t + 301 * S), Step::Probe);
    }

    #[test]
    fn what_the_page_holds_keeps_it_until_asked_again() {
        let t = Instant::now();
        let p = policy(10, false);
        let none = Leases::default();
        for (answer, kept) in [
            (
                ProbeAnswer {
                    watched: true,
                    audible: true,
                    ..Default::default()
                },
                Lease::Audio,
            ),
            (
                ProbeAnswer {
                    watched: true,
                    edited: true,
                    ..Default::default()
                },
                Lease::EditedForm,
            ),
            (
                ProbeAnswer {
                    watched: true,
                    before_unload: true,
                    ..Default::default()
                },
                Lease::BeforeUnload,
            ),
            (ProbeAnswer::default(), Lease::Unknown),
        ] {
            let mut life = Life::default();
            life.tick(none, false, &p, t);
            assert_eq!(life.tick(none, false, &p, t + 10 * S), Step::Probe);
            life.probed(Some(answer), t + 10 * S);
            assert_eq!(
                life.tick(none, false, &p, t + 11 * S),
                Step::Stay,
                "{kept:?}"
            );
            assert!(life.held(none).has(kept));
            // A minute on, it is asked again; now it holds nothing.
            assert_eq!(life.tick(none, false, &p, t + 70 * S), Step::Probe);
            life.probed(clean(), t + 70 * S);
            assert_eq!(life.tick(none, false, &p, t + 71 * S), Step::Discard);
        }
        // No answer in time keeps it too.
        let mut life = Life::default();
        life.tick(none, false, &p, t);
        life.probed(None, t + 10 * S);
        assert_eq!(life.tick(none, false, &p, t + 11 * S), Step::Stay);
    }

    #[test]
    fn memory_saver_off_never_discards() {
        let t = Instant::now();
        let p = Policy::new(0, None, true, false);
        let mut life = Life::default();
        life.tick(Leases::default(), false, &p, t);
        assert_eq!(
            life.tick(Leases::default(), false, &p, t + 100_000 * S),
            Step::Stay
        );
    }

    #[test]
    fn a_low_battery_freezes_idle_pages_and_a_lease_thaws_them() {
        let t = Instant::now();
        let p = policy(1000, true);
        let none = Leases::default();
        let mut life = Life::default();
        life.tick(none, false, &p, t);
        assert_eq!(life.tick(none, false, &p, t + 9 * S), Step::Stay);
        assert_eq!(life.tick(none, false, &p, t + 10 * S), Step::Probe);
        // An edited form survives freezing.
        life.probed(
            Some(ProbeAnswer {
                watched: true,
                edited: true,
                ..Default::default()
            }),
            t + 10 * S,
        );
        assert_eq!(life.tick(none, false, &p, t + 11 * S), Step::Freeze);
        assert_eq!(life.tick(none, true, &p, t + 12 * S), Step::Stay);
        // Frozen, its last answer holds past a minute: it ran nothing since.
        assert_eq!(life.tick(none, true, &p, t + 999 * S), Step::Stay);
        // The edited form still keeps it from being discarded.
        assert_eq!(life.tick(none, true, &p, t + 1100 * S), Step::Stay);
        // Shown again: it thaws.
        let shown = Leases::default().with(Lease::Shown, true);
        assert_eq!(life.tick(shown, true, &p, t + 1101 * S), Step::Thaw);
        // Back on power, a frozen page thaws too.
        let mut life = Life::default();
        let power = policy(1000, false);
        assert_eq!(life.tick(none, true, &power, t), Step::Thaw);
    }

    #[test]
    fn sound_or_silence_keeps_a_page_awake() {
        let t = Instant::now();
        let p = policy(1000, true);
        let none = Leases::default();
        let mut life = Life::default();
        life.tick(none, false, &p, t);
        life.probed(
            Some(ProbeAnswer {
                watched: true,
                audible: true,
                ..Default::default()
            }),
            t + 10 * S,
        );
        assert_eq!(life.tick(none, false, &p, t + 11 * S), Step::Stay);
        let mut life = Life::default();
        life.tick(none, false, &p, t);
        life.probed(None, t + 10 * S);
        assert_eq!(life.tick(none, false, &p, t + 11 * S), Step::Stay);
    }
}
