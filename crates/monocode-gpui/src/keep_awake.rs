//! Keep computer awake, as Amphetamine does it: an IOKit power assertion
//! held for as long as the mode asks for one. "On" holds it always, "Agent"
//! while an agent is working, "Off" never.
//!
//! The assertion is the kernel's, so it ends with the process: a crash or a
//! force quit never leaves the Mac unable to sleep. `pmset -g assertions`
//! lists it under the app's name.
//!
//! The footer's chip and the menu bar icon both read and set the one
//! `KeepAwake` global; the shell feeds it the working agents once a second.
use gpui::{App, Global};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A working agent whose transcript has not changed for this long no longer
/// keeps the computer awake: it is more likely hung than working.
const STALE_AFTER: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    On,
    Agent,
    #[default]
    Off,
}

impl Mode {
    pub(crate) const ALL: [Mode; 3] = [Mode::On, Mode::Agent, Mode::Off];

    /// The saved preference; anything unknown is Off.
    pub(crate) fn parse(value: &str) -> Self {
        match value {
            "on" => Self::On,
            "agent" => Self::Agent,
            _ => Self::Off,
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Agent => "agent",
            Self::Off => "off",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::On => "On",
            Self::Agent => "Agent",
            Self::Off => "Off",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::On => "Keep this computer awake continuously",
            Self::Agent => "Stay awake while an agent is working",
            Self::Off => "Allow normal system sleep behavior",
        }
    }
}

/// Whether the computer should be kept awake.
pub(crate) fn wanted(mode: Mode, working: usize) -> bool {
    match mode {
        Mode::On => true,
        Mode::Agent => working > 0,
        Mode::Off => false,
    }
}

/// What the footer and the menu bar show: the mode and whether it holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Status {
    pub mode: Mode,
    pub active: bool,
    pub working: usize,
    /// Running on battery, where macOS sleeps on a closed lid anyway.
    pub on_battery: bool,
}

impl Status {
    /// "Agent · Active", as the menu's heading says it.
    pub(crate) fn summary(self) -> String {
        format!(
            "{} · {}",
            self.mode.label(),
            if self.active { "Active" } else { "Inactive" }
        )
    }

    /// On battery a closed lid sleeps the Mac whatever the assertion says:
    /// only a root `pmset disablesleep` gets past it, which this does not do.
    pub(crate) fn lid_warning(self) -> Option<&'static str> {
        (self.mode != Mode::Off && self.on_battery)
            .then_some("On battery, closing the lid still puts the Mac to sleep")
    }
}

/// A working agent's transcript: rows and bytes, which change as it works.
pub(crate) type Mark = (usize, usize);

#[derive(Default)]
pub(crate) struct KeepAwake {
    mode: Mode,
    /// Working agents by session: their last mark and when it changed.
    seen: HashMap<i64, (Mark, Instant)>,
    working: usize,
    on_battery: bool,
    assertion: Option<platform::Assertion>,
}

impl Global for KeepAwake {}

impl KeepAwake {
    pub(crate) fn status(&self) -> Status {
        Status {
            mode: self.mode,
            active: self.assertion.is_some(),
            working: self.working,
            on_battery: self.on_battery,
        }
    }

    /// Sets the mode, and takes or releases the assertion to match.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.apply();
    }

    /// Takes the working agents now: their sessions and transcript marks,
    /// and whether the Mac runs on battery.
    /// An agent whose mark has not moved in `STALE_AFTER` is not counted.
    pub(crate) fn observe(
        &mut self,
        now: Instant,
        agents: impl IntoIterator<Item = (i64, Mark)>,
        on_battery: bool,
    ) {
        self.on_battery = on_battery;
        let mut seen = HashMap::new();
        for (id, mark) in agents {
            let since = match self.seen.get(&id) {
                Some((last, since)) if *last == mark => *since,
                _ => now,
            };
            seen.insert(id, (mark, since));
        }
        self.working = seen
            .values()
            .filter(|(_, since)| now.duration_since(*since) < STALE_AFTER)
            .count();
        self.seen = seen;
        self.apply();
    }

    fn apply(&mut self) {
        let wanted = wanted(self.mode, self.working);
        if wanted && self.assertion.is_none() {
            self.assertion = platform::Assertion::take();
            if self.assertion.is_none() {
                log::warn!("keep awake: could not take the power assertion");
            }
        } else if !wanted {
            // Dropping releases it.
            self.assertion = None;
        }
    }
}

/// Whether the Mac draws from its battery now.
pub(crate) fn on_battery() -> bool {
    platform::on_battery()
}

/// The global's status, Off until the shell first sets the mode.
pub(crate) fn status(cx: &App) -> Status {
    cx.try_global::<KeepAwake>()
        .map(KeepAwake::status)
        .unwrap_or_default()
}

#[cfg(all(target_os = "macos", not(test)))]
mod platform {
    //! IOPMAssertionCreateWithName, the call Amphetamine and `caffeinate`
    //! make. CoreFoundation strings are made by hand: two calls are not worth
    //! a crate.
    use std::ffi::{CString, c_char, c_void};

    type CFStringRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    /// kIOPMAssertionLevelOn.
    const LEVEL_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(object: *const c_void);
        fn CFEqual(a: *const c_void, b: *const c_void) -> u8;
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
        fn IOPSCopyPowerSourcesInfo() -> *const c_void;
        fn IOPSGetProvidingPowerSourceType(snapshot: *const c_void) -> CFStringRef;
    }

    /// kIOPMBatteryPowerKey is the providing source; a Mac without a battery
    /// says "AC Power".
    pub(super) fn on_battery() -> bool {
        let Some(battery) = CfString::new("Battery Power") else {
            return false;
        };
        unsafe {
            let snapshot = IOPSCopyPowerSourcesInfo();
            if snapshot.is_null() {
                return false;
            }
            // Owned by the snapshot: not released on its own.
            let source = IOPSGetProvidingPowerSourceType(snapshot);
            let on = !source.is_null() && CFEqual(source, battery.0) != 0;
            CFRelease(snapshot);
            on
        }
    }

    struct CfString(CFStringRef);
    impl CfString {
        fn new(text: &str) -> Option<Self> {
            let text = CString::new(text).ok()?;
            let string =
                unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) };
            (!string.is_null()).then_some(Self(string))
        }
    }
    impl Drop for CfString {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }

    fn assert(kind: &str) -> Option<u32> {
        let kind = CfString::new(kind)?;
        let name = CfString::new("MonoCode: Keep computer awake")?;
        let mut id = 0;
        let result = unsafe { IOPMAssertionCreateWithName(kind.0, LEVEL_ON, name.0, &mut id) };
        (result == 0).then_some(id)
    }

    pub(super) struct Assertion(Vec<u32>);
    impl Assertion {
        /// Idle sleep is held off always; on AC power, closing the lid too
        /// (macOS ignores this second one on battery).
        pub(super) fn take() -> Option<Self> {
            let idle = assert("PreventUserIdleSystemSleep")?;
            let mut ids = vec![idle];
            ids.extend(assert("PreventSystemSleep"));
            Some(Self(ids))
        }
    }
    impl Drop for Assertion {
        fn drop(&mut self) {
            for id in &self.0 {
                unsafe { IOPMAssertionRelease(*id) };
            }
        }
    }
}

#[cfg(not(all(target_os = "macos", not(test))))]
mod platform {
    //! Elsewhere, and in tests, the assertion is only remembered.
    pub(super) struct Assertion;
    impl Assertion {
        pub(super) fn take() -> Option<Self> {
            Some(Self)
        }
    }
    pub(super) fn on_battery() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip_and_unknown_is_off() {
        for mode in Mode::ALL {
            assert_eq!(Mode::parse(mode.key()), mode);
        }
        assert_eq!(Mode::parse(""), Mode::Off);
        assert_eq!(Mode::parse("always"), Mode::Off);
    }

    #[test]
    fn wanted_follows_mode_and_agents() {
        assert!(wanted(Mode::On, 0));
        assert!(!wanted(Mode::Agent, 0));
        assert!(wanted(Mode::Agent, 2));
        assert!(!wanted(Mode::Off, 3));
    }

    #[test]
    fn agent_mode_holds_only_while_agents_work() {
        let mut awake = KeepAwake::default();
        let now = Instant::now();
        awake.set_mode(Mode::Agent);
        assert!(!awake.status().active);
        awake.observe(now, [(1, (3, 10))], false);
        assert_eq!(awake.status().working, 1);
        assert!(awake.status().active);
        awake.observe(now, [], false);
        assert!(!awake.status().active);
        assert_eq!(
            awake.status().summary(),
            "Agent · Inactive",
            "the menu heading"
        );
    }

    #[test]
    fn off_releases_and_on_holds_without_agents() {
        let mut awake = KeepAwake::default();
        awake.set_mode(Mode::On);
        assert!(awake.status().active);
        assert_eq!(awake.status().summary(), "On · Active");
        awake.set_mode(Mode::Off);
        assert!(!awake.status().active);
    }

    #[test]
    fn the_lid_warning_shows_on_battery_unless_off() {
        let mut awake = KeepAwake::default();
        awake.set_mode(Mode::Agent);
        awake.observe(Instant::now(), [], false);
        assert_eq!(awake.status().lid_warning(), None, "on AC power");
        awake.observe(Instant::now(), [], true);
        assert!(awake.status().lid_warning().is_some(), "Agent, even idle");
        awake.set_mode(Mode::Off);
        assert_eq!(awake.status().lid_warning(), None);
    }

    #[test]
    fn an_agent_quiet_for_two_hours_stops_counting() {
        let mut awake = KeepAwake::default();
        awake.set_mode(Mode::Agent);
        let start = Instant::now();
        awake.observe(start, [(7, (5, 100))], false);
        let later = start + STALE_AFTER - Duration::from_secs(1);
        awake.observe(later, [(7, (5, 100))], false);
        assert!(awake.status().active, "still within the window");
        awake.observe(start + STALE_AFTER, [(7, (5, 100))], false);
        assert!(!awake.status().active, "no change in two hours");
        // Output again: it counts again from now.
        awake.observe(start + STALE_AFTER, [(7, (6, 140))], false);
        assert!(awake.status().active);
    }
}
