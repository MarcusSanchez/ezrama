//! Policy for the resident watcher: what the events mean, what state it
//! reports, and how long to wait before trying again after a lost session.

use std::time::Duration;

/// First pause before retrying a session start after a loss.
pub const RECONNECT_INITIAL: Duration = Duration::from_millis(3000);
/// Longest pause between session start attempts.
pub const RECONNECT_MAX: Duration = Duration::from_millis(30_000);

/// A change in the display's presence, or a control request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The display's printer interface appeared at this path.
    Arrived(String),
    /// The display's printer interface at this path went away.
    Removed(String),
    /// Release the display and stay idle until resumed.
    Pause,
    /// Take the display back after a pause.
    Resume,
    /// Release the display, start KANALI, and take the display back once
    /// KANALI has exited.
    OpenKanali,
    /// The KANALI started on request has exited.
    KanaliClosed,
    /// The watcher should release the display and exit.
    Quit,
}

/// What the supervisor should do after handling an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directive {
    /// Carry on with the current mode.
    Continue,
    /// Release everything and exit.
    Quit,
}

/// What the watcher is doing, as shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Not yet looked for the display.
    Starting = 0,
    /// A session is up and the display shows its media.
    Active = 1,
    /// The display is present and a session is being started.
    Connecting = 2,
    /// Released on request; idle until resumed.
    Paused = 3,
    /// Released while a KANALI started on request runs.
    WaitingForKanali = 4,
    /// No display, or more than one, is present.
    NoDisplay = 5,
    /// Releasing everything before exit.
    Quitting = 6,
}

impl State {
    /// The wording shown in the menu and the tooltip.
    pub fn label(self) -> &'static str {
        match self {
            State::Starting => "Starting",
            State::Active => "Active",
            State::Connecting => "Connecting",
            State::Paused => "Paused",
            State::WaitingForKanali => "Waiting for KANALI",
            State::NoDisplay => "No display found",
            State::Quitting => "Quitting",
        }
    }

    /// Whether the display has been released on request.
    pub fn released(self) -> bool {
        matches!(self, State::Paused | State::WaitingForKanali)
    }

    /// The state for a code produced by `as u8`.
    pub fn from_code(code: u8) -> Option<State> {
        [
            State::Starting,
            State::Active,
            State::Connecting,
            State::Paused,
            State::WaitingForKanali,
            State::NoDisplay,
            State::Quitting,
        ]
        .into_iter()
        .find(|state| *state as u8 == code)
    }
}

/// The watcher's control state: whether it may hold the display, and
/// whether a KANALI launch is owed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    paused: bool,
    launched: bool,
    launch_pending: bool,
}

impl Control {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the watcher has been asked to leave the display alone.
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Whether the pause is on behalf of a KANALI started on request.
    pub fn waiting_for_kanali(&self) -> bool {
        self.launched
    }

    /// The state to report while paused.
    pub fn paused_state(&self) -> State {
        if self.launched {
            State::WaitingForKanali
        } else {
            State::Paused
        }
    }

    /// Whether KANALI should be started now; asks once per request.
    pub fn take_launch(&mut self) -> bool {
        std::mem::take(&mut self.launch_pending)
    }

    /// Applies one event and says whether to keep going.
    pub fn apply(&mut self, event: &Event) -> Directive {
        match event {
            Event::Pause => {
                self.paused = true;
                self.launched = false;
                self.launch_pending = false;
            }
            Event::Resume => {
                self.paused = false;
                self.launched = false;
                self.launch_pending = false;
            }
            Event::OpenKanali => {
                self.paused = true;
                self.launched = true;
                self.launch_pending = true;
            }
            Event::KanaliClosed => {
                if self.launched {
                    self.paused = false;
                    self.launched = false;
                }
                self.launch_pending = false;
            }
            Event::Quit => return Directive::Quit,
            Event::Arrived(_) | Event::Removed(_) => {}
        }
        Directive::Continue
    }
}

/// Grows the pause between session start attempts while the display stays
/// present but the session keeps failing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reconnect {
    attempt: u32,
}

impl Reconnect {
    pub fn new() -> Self {
        Self::default()
    }

    /// The pause before the next attempt: 3, 6, 12, 24, then 30 s.
    pub fn next_delay(&mut self) -> Duration {
        let delay = (RECONNECT_INITIAL * (1u32 << self.attempt.min(4))).min(RECONNECT_MAX);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    /// Attempts made since the last reset.
    pub fn attempts(&self) -> u32 {
        self.attempt
    }

    /// Forgets the failures after a session has been established.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_delays_double_and_cap() {
        let mut reconnect = Reconnect::new();
        let delays: Vec<u64> = (0..7).map(|_| reconnect.next_delay().as_millis() as u64).collect();
        assert_eq!(delays, [3000, 6000, 12_000, 24_000, 30_000, 30_000, 30_000]);
        assert_eq!(reconnect.attempts(), 7);
        reconnect.reset();
        assert_eq!(reconnect.attempts(), 0);
        assert_eq!(reconnect.next_delay(), RECONNECT_INITIAL);
    }

    #[test]
    fn control_tracks_pause_and_resume() {
        let mut control = Control::new();
        assert!(!control.paused());
        assert_eq!(control.apply(&Event::Arrived("a".into())), Directive::Continue);
        assert!(!control.paused());
        assert_eq!(control.apply(&Event::Pause), Directive::Continue);
        assert!(control.paused());
        assert_eq!(control.paused_state(), State::Paused);
        assert_eq!(control.apply(&Event::Removed("a".into())), Directive::Continue);
        assert!(control.paused());
        assert_eq!(control.apply(&Event::Pause), Directive::Continue);
        assert!(control.paused());
        assert_eq!(control.apply(&Event::Resume), Directive::Continue);
        assert!(!control.paused());
        assert_eq!(control.apply(&Event::Quit), Directive::Quit);
    }

    #[test]
    fn a_launch_pauses_once_and_resumes_when_kanali_closes() {
        let mut control = Control::new();
        assert!(!control.take_launch());
        assert_eq!(control.apply(&Event::OpenKanali), Directive::Continue);
        assert!(control.paused());
        assert!(control.waiting_for_kanali());
        assert_eq!(control.paused_state(), State::WaitingForKanali);
        assert!(control.take_launch());
        assert!(!control.take_launch());
        assert_eq!(control.apply(&Event::KanaliClosed), Directive::Continue);
        assert!(!control.paused());
        assert!(!control.waiting_for_kanali());
    }

    #[test]
    fn a_manual_pause_or_resume_outlives_the_launch() {
        let mut control = Control::new();
        control.apply(&Event::OpenKanali);
        control.apply(&Event::Pause);
        assert!(control.paused());
        assert!(!control.waiting_for_kanali());
        assert!(!control.take_launch());
        control.apply(&Event::KanaliClosed);
        assert!(control.paused(), "a manual pause is not ended by KANALI closing");

        let mut control = Control::new();
        control.apply(&Event::OpenKanali);
        control.take_launch();
        control.apply(&Event::Resume);
        assert!(!control.paused());
        control.apply(&Event::KanaliClosed);
        assert!(!control.paused());

        let mut control = Control::new();
        control.apply(&Event::KanaliClosed);
        assert!(!control.paused(), "a stray close changes nothing");
    }

    #[test]
    fn states_round_trip_through_their_codes_and_have_labels() {
        for code in 0..=6u8 {
            let state = State::from_code(code).unwrap();
            assert_eq!(state as u8, code);
            assert!(!state.label().is_empty());
        }
        assert_eq!(State::from_code(7), None);
        assert!(State::Paused.released());
        assert!(State::WaitingForKanali.released());
        assert!(!State::Active.released());
        assert_eq!(State::WaitingForKanali.label(), "Waiting for KANALI");
    }
}
