//! The resident watcher's supervisor: which mode it is in, what each event
//! means there, and when to try the display again. The machine is behind
//! [`Backend`], so the policy runs, and is tested, anywhere.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::log::Logger;
use crate::session::SessionError;
use crate::watch::{Control, Directive, Event, Reconnect, State};

/// What a look for the display found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Panel {
    /// Exactly one display, at this interface path.
    One(String),
    Absent,
    /// More than one display; the count.
    Several(usize),
}

/// Why a session start did not produce a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// An event cut the attempt short; nothing is wrong with the display.
    Interrupted,
    /// The attempt failed for this reason.
    Failed(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Interrupted => write!(f, "session start interrupted"),
            StartError::Failed(reason) => write!(f, "{reason}"),
        }
    }
}

/// How a held session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// Stopped on request after this many pings.
    Stopped { pings: u64 },
    /// Lost after this many pings.
    Lost { pings: u64, error: SessionError },
}

/// What the supervisor needs from the machine.
pub trait Backend {
    /// Whether a stop has been asked for outside the event channel.
    fn stop_requested(&self) -> bool;
    /// Forgets a pending interrupt once its events have been handled.
    fn clear_interrupt(&mut self);
    /// Tells the machine whether the display is released on request.
    fn set_released(&mut self, released: bool);
    /// Reports the state the watcher is in.
    fn report(&mut self, state: State);
    /// Looks for the display.
    fn find_panel(&mut self) -> Result<Panel, String>;
    /// Opens the display at `path` and runs the session start, logging
    /// each step.
    fn start_session(&mut self, path: &str, log: &mut Logger) -> Result<(), StartError>;
    /// Holds the started session until interrupted or lost, then releases
    /// it.
    fn hold_session(&mut self, interval: Duration, verbose: bool, log: &mut Logger) -> Hold;
    /// Starts KANALI, arranging for [`Event::KanaliClosed`] once it has
    /// exited. False when it could not be started.
    fn start_kanali(&mut self, log: &mut Logger) -> bool;
}

/// Keeps a session whenever the display is present, releases it on
/// request, and retries a failed start with growing pauses.
pub struct Supervisor<B: Backend> {
    backend: B,
    log: Logger,
    interval: Duration,
    verbose: bool,
    control: Control,
    reconnect: Reconnect,
    announced: Option<State>,
}

impl<B: Backend> Supervisor<B> {
    pub fn new(backend: B, log: Logger, interval: Duration, verbose: bool) -> Self {
        Self::with_reconnect(backend, log, interval, verbose, Reconnect::new())
    }

    /// A supervisor with its own retry schedule.
    pub fn with_reconnect(
        backend: B,
        log: Logger,
        interval: Duration,
        verbose: bool,
        reconnect: Reconnect,
    ) -> Self {
        Self {
            backend,
            log,
            interval,
            verbose,
            control: Control::new(),
            reconnect,
            announced: None,
        }
    }

    /// Runs until asked to quit, then hands back the backend and the log
    /// for the caller to finish with.
    pub fn run(mut self, events: &Receiver<Event>) -> (B, Logger) {
        loop {
            if self.backend.stop_requested() || self.drain(events) == Directive::Quit {
                break;
            }
            self.backend.clear_interrupt();
            let directive = if self.control.paused() {
                self.paused(events)
            } else {
                self.active(events)
            };
            if directive == Directive::Quit {
                break;
            }
        }
        self.backend.report(State::Quitting);
        self.log.log("watch stopping");
        (self.backend, self.log)
    }

    /// Applies one event to the control state and logs it.
    fn apply(&mut self, event: Event) -> Directive {
        match &event {
            Event::Arrived(path) => self.log.log(&format!("panel arrived: {path}")),
            Event::Removed(path) => self.log.log(&format!("panel removed: {path}")),
            Event::Pause => self.log.log("pause requested"),
            Event::Resume => self.log.log("resume requested"),
            Event::OpenKanali => self.log.log("KANALI requested"),
            Event::KanaliClosed => self.log.log("KANALI has exited"),
            Event::StartupSet { enabled, result } => {
                let wording = if *enabled { "on" } else { "off" };
                self.log.log(&match result {
                    Ok(true) => format!("start with Windows turned {wording}"),
                    Ok(false) => format!("start with Windows was already {wording}"),
                    Err(reason) => format!("start with Windows not changed: {reason}"),
                });
            }
            Event::Quit => self.log.log("stop requested"),
        }
        self.control.apply(&event)
    }

    /// Handles queued events without waiting.
    fn drain(&mut self, events: &Receiver<Event>) -> Directive {
        while let Ok(event) = events.try_recv() {
            if self.apply(event) == Directive::Quit {
                return Directive::Quit;
            }
        }
        Directive::Continue
    }

    /// Waits for one event, or for `timeout` when given. A vanished event
    /// source counts as a quit.
    fn wait(&mut self, events: &Receiver<Event>, timeout: Option<Duration>) -> Directive {
        let event = match timeout {
            None => match events.recv() {
                Ok(event) => event,
                Err(_) => return Directive::Quit,
            },
            Some(timeout) => match events.recv_timeout(timeout) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => return Directive::Continue,
                Err(RecvTimeoutError::Disconnected) => return Directive::Quit,
            },
        };
        self.apply(event)
    }

    /// One turn of the released mode: the launch if one is owed, then a
    /// wait for the next event.
    fn paused(&mut self, events: &Receiver<Event>) -> Directive {
        self.backend.set_released(true);
        if self.control.take_launch() && !self.backend.start_kanali(&mut self.log) {
            self.control.apply(&Event::KanaliClosed);
            return Directive::Continue;
        }
        let state = self.control.paused_state();
        self.backend.report(state);
        if self.announced != Some(state) {
            self.log.log(match state {
                State::WaitingForKanali => "paused until KANALI exits",
                _ => "paused: the panel is released until resume",
            });
            self.announced = Some(state);
        }
        self.wait(events, None)
    }

    /// One turn of the holding mode: find the display, start a session,
    /// hold it until something happens.
    fn active(&mut self, events: &Receiver<Event>) -> Directive {
        self.backend.set_released(false);
        self.announced = None;

        let path = match self.backend.find_panel() {
            Ok(Panel::One(path)) => path,
            Ok(Panel::Absent) => {
                self.backend.report(State::NoDisplay);
                self.log.log("panel not present; waiting");
                return self.wait(events, None);
            }
            Ok(Panel::Several(count)) => {
                self.backend.report(State::NoDisplay);
                self.log.log(&format!("{count} panels present; waiting for exactly one"));
                return self.wait(events, None);
            }
            Err(error) => {
                self.backend.report(State::Connecting);
                self.log.log(&format!("discovery failed: {error}"));
                let delay = self.reconnect.next_delay();
                return self.wait(events, Some(delay));
            }
        };

        self.backend.report(State::Connecting);
        match self.backend.start_session(&path, &mut self.log) {
            Ok(()) => {}
            Err(StartError::Interrupted) => {
                self.log.log("session start interrupted");
                return Directive::Continue;
            }
            Err(StartError::Failed(message)) => {
                let delay = self.reconnect.next_delay();
                self.log.log(&format!("{message}; retrying in {} s", delay.as_secs()));
                return self.wait(events, Some(delay));
            }
        }
        self.reconnect.reset();
        self.backend.report(State::Active);
        self.log.log(&format!("holding: ping every {} ms", self.interval.as_millis()));

        match self.backend.hold_session(self.interval, self.verbose, &mut self.log) {
            Hold::Stopped { pings } => {
                self.log.log(&format!("session released after {pings} pings"));
                Directive::Continue
            }
            Hold::Lost { pings, error } => {
                self.backend.report(State::Connecting);
                let delay = self.reconnect.next_delay();
                self.log.log(&format!(
                    "session lost after {pings} pings: {error}; retrying in {} s",
                    delay.as_secs()
                ));
                self.wait(events, Some(delay))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::ReadError;
    use std::collections::VecDeque;
    use std::sync::mpsc::{self, Sender};

    /// A scripted machine. Each queue answers the calls of one kind in
    /// order; `on_report` sends events into the channel each time a state
    /// is reported, which is how a test drives the supervisor along.
    struct Fake {
        sender: Sender<Event>,
        panels: VecDeque<Result<Panel, String>>,
        starts: VecDeque<Result<(), StartError>>,
        holds: VecDeque<Hold>,
        launches: VecDeque<bool>,
        on_report: VecDeque<Vec<Event>>,
        stop: bool,
        states: Vec<State>,
        calls: Vec<String>,
        released: Vec<bool>,
    }

    impl Fake {
        fn new(sender: Sender<Event>) -> Self {
            Self {
                sender,
                panels: VecDeque::new(),
                starts: VecDeque::new(),
                holds: VecDeque::new(),
                launches: VecDeque::new(),
                on_report: VecDeque::new(),
                stop: false,
                states: Vec::new(),
                calls: Vec::new(),
                released: Vec::new(),
            }
        }

        fn panel(mut self, panel: Result<Panel, String>) -> Self {
            self.panels.push_back(panel);
            self
        }

        fn start(mut self, outcome: Result<(), StartError>) -> Self {
            self.starts.push_back(outcome);
            self
        }

        fn hold(mut self, outcome: Hold) -> Self {
            self.holds.push_back(outcome);
            self
        }

        fn launch(mut self, started: bool) -> Self {
            self.launches.push_back(started);
            self
        }

        /// Events sent when the next state is reported.
        fn then(mut self, events: Vec<Event>) -> Self {
            self.on_report.push_back(events);
            self
        }

        /// The release flag with consecutive repeats collapsed.
        fn release_changes(&self) -> Vec<bool> {
            let mut changes: Vec<bool> = Vec::new();
            for &flag in &self.released {
                if changes.last() != Some(&flag) {
                    changes.push(flag);
                }
            }
            changes
        }
    }

    impl Backend for Fake {
        fn stop_requested(&self) -> bool {
            self.stop
        }

        fn clear_interrupt(&mut self) {}

        fn set_released(&mut self, released: bool) {
            self.released.push(released);
        }

        fn report(&mut self, state: State) {
            self.states.push(state);
            for event in self.on_report.pop_front().unwrap_or_default() {
                self.sender.send(event).unwrap();
            }
        }

        fn find_panel(&mut self) -> Result<Panel, String> {
            self.calls.push("find".into());
            self.panels.pop_front().expect("the script has no more panels")
        }

        fn start_session(&mut self, path: &str, _log: &mut Logger) -> Result<(), StartError> {
            self.calls.push(format!("start {path}"));
            self.starts.pop_front().expect("the script has no more starts")
        }

        fn hold_session(&mut self, interval: Duration, _verbose: bool, _log: &mut Logger) -> Hold {
            self.calls.push(format!("hold {}", interval.as_millis()));
            self.holds.pop_front().expect("the script has no more holds")
        }

        fn start_kanali(&mut self, _log: &mut Logger) -> bool {
            self.calls.push("launch".into());
            self.launches.pop_front().expect("the script has no more launches")
        }
    }

    const INTERVAL: Duration = Duration::from_millis(4500);

    fn run(fake: Fake, events: &Receiver<Event>) -> Fake {
        let quick = Reconnect::with_unit(Duration::from_millis(1));
        let supervisor = Supervisor::with_reconnect(fake, Logger::silent(), INTERVAL, false, quick);
        supervisor.run(events).0
    }

    fn one() -> Result<Panel, String> {
        Ok(Panel::One("panel".into()))
    }

    fn lost(pings: u64) -> Hold {
        Hold::Lost {
            pings,
            error: SessionError::Read(ReadError::Lost("unplugged".into())),
        }
    }

    #[test]
    fn holds_the_panel_until_asked_to_stop() {
        let (sender, events) = mpsc::channel();
        let fake = Fake::new(sender)
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::Quit])
            .hold(Hold::Stopped { pings: 3 });
        let fake = run(fake, &events);
        assert_eq!(fake.states, [State::Connecting, State::Active, State::Quitting]);
        assert_eq!(fake.calls, ["find", "start panel", "hold 4500"]);
        assert_eq!(fake.release_changes(), [false]);
    }

    #[test]
    fn a_lost_session_is_retried_and_a_failed_start_backs_off() {
        let (sender, events) = mpsc::channel();
        let fake = Fake::new(sender)
            .panel(one())
            .start(Ok(()))
            .hold(lost(7))
            .panel(one())
            .start(Err(StartError::Failed("session start failed".into())))
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![])
            .then(vec![])
            .then(vec![])
            .then(vec![])
            .then(vec![Event::Quit])
            .hold(Hold::Stopped { pings: 1 });
        let fake = run(fake, &events);
        assert_eq!(
            fake.states,
            [
                State::Connecting,
                State::Active,
                State::Connecting,
                State::Connecting,
                State::Connecting,
                State::Active,
                State::Quitting
            ]
        );
        assert_eq!(
            fake.calls,
            ["find", "start panel", "hold 4500", "find", "start panel", "find", "start panel", "hold 4500"]
        );
    }

    #[test]
    fn a_pause_releases_and_a_resume_reconnects() {
        let (sender, events) = mpsc::channel();
        let fake = Fake::new(sender)
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::Pause])
            .hold(Hold::Stopped { pings: 2 })
            .then(vec![Event::Resume])
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::Quit])
            .hold(Hold::Stopped { pings: 0 });
        let fake = run(fake, &events);
        assert_eq!(
            fake.states,
            [State::Connecting, State::Active, State::Paused, State::Connecting, State::Active, State::Quitting]
        );
        assert_eq!(fake.release_changes(), [false, true, false]);
        assert!(!fake.calls.contains(&"launch".to_string()));
    }

    #[test]
    fn open_kanali_launches_once_and_resumes_when_it_closes() {
        let (sender, events) = mpsc::channel();
        let fake = Fake::new(sender)
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::OpenKanali])
            .hold(Hold::Stopped { pings: 5 })
            .launch(true)
            .then(vec![Event::KanaliClosed])
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::Quit])
            .hold(Hold::Stopped { pings: 0 });
        let fake = run(fake, &events);
        assert_eq!(
            fake.states,
            [
                State::Connecting,
                State::Active,
                State::WaitingForKanali,
                State::Connecting,
                State::Active,
                State::Quitting
            ]
        );
        assert_eq!(fake.calls.iter().filter(|call| *call == "launch").count(), 1);
        assert_eq!(fake.release_changes(), [false, true, false]);
    }

    #[test]
    fn a_failed_launch_resumes_at_once() {
        let (sender, events) = mpsc::channel();
        let fake = Fake::new(sender)
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::OpenKanali])
            .hold(Hold::Stopped { pings: 5 })
            .launch(false)
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::Quit])
            .hold(Hold::Stopped { pings: 0 });
        let fake = run(fake, &events);
        assert_eq!(
            fake.states,
            [State::Connecting, State::Active, State::Connecting, State::Active, State::Quitting]
        );
        assert_eq!(fake.release_changes(), [false, true, false]);
    }

    #[test]
    fn a_manual_pause_during_the_wait_outlives_the_close() {
        let (sender, events) = mpsc::channel();
        let fake = Fake::new(sender)
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::OpenKanali])
            .hold(Hold::Stopped { pings: 5 })
            .launch(true)
            .then(vec![Event::Pause])
            .then(vec![Event::KanaliClosed])
            .then(vec![Event::Quit]);
        let fake = run(fake, &events);
        assert_eq!(
            fake.states,
            [
                State::Connecting,
                State::Active,
                State::WaitingForKanali,
                State::Paused,
                State::Paused,
                State::Quitting
            ]
        );
        assert_eq!(fake.calls.len(), 4, "no reconnect after the close");
    }

    #[test]
    fn an_absent_or_ambiguous_panel_waits_for_an_event() {
        let (sender, events) = mpsc::channel();
        let fake = Fake::new(sender)
            .panel(Ok(Panel::Absent))
            .then(vec![Event::Arrived("panel".into())])
            .panel(Ok(Panel::Several(2)))
            .then(vec![Event::Removed("other".into())])
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::Quit])
            .hold(Hold::Stopped { pings: 0 });
        let fake = run(fake, &events);
        assert_eq!(
            fake.states,
            [State::NoDisplay, State::NoDisplay, State::Connecting, State::Active, State::Quitting]
        );
        assert_eq!(fake.calls, ["find", "find", "find", "start panel", "hold 4500"]);
    }

    #[test]
    fn an_interrupted_start_is_not_a_failure() {
        let (sender, events) = mpsc::channel();
        let fake = Fake::new(sender)
            .panel(one())
            .then(vec![Event::Pause])
            .start(Err(StartError::Interrupted))
            .then(vec![Event::Resume])
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![Event::Quit])
            .hold(Hold::Stopped { pings: 0 });
        let fake = run(fake, &events);
        assert_eq!(
            fake.states,
            [State::Connecting, State::Paused, State::Connecting, State::Active, State::Quitting]
        );
        assert_eq!(fake.calls, ["find", "start panel", "find", "start panel", "hold 4500"]);
    }

    #[test]
    fn a_startup_change_is_only_logged_and_changes_no_mode() {
        let (sender, events) = mpsc::channel();
        let set = |enabled: bool, result: Result<bool, String>| Event::StartupSet { enabled, result };
        let fake = Fake::new(sender)
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![
                set(false, Ok(true)),
                set(true, Ok(false)),
                set(true, Err("denied".into())),
                Event::Quit,
            ])
            .hold(Hold::Stopped { pings: 1 });
        let fake = run(fake, &events);
        assert_eq!(fake.states, [State::Connecting, State::Active, State::Quitting]);
        assert_eq!(fake.calls, ["find", "start panel", "hold 4500"]);
    }

    #[test]
    fn a_discovery_error_is_retried_after_a_pause() {
        let (sender, events) = mpsc::channel();
        let fake = Fake::new(sender)
            .panel(Err("enumeration failed".into()))
            .panel(one())
            .start(Ok(()))
            .then(vec![])
            .then(vec![])
            .then(vec![Event::Quit])
            .hold(Hold::Stopped { pings: 0 });
        let fake = run(fake, &events);
        assert_eq!(fake.states, [State::Connecting, State::Connecting, State::Active, State::Quitting]);
    }

    #[test]
    fn a_stop_from_the_console_ends_the_run_before_anything_else() {
        let (sender, events) = mpsc::channel();
        let mut fake = Fake::new(sender);
        fake.stop = true;
        let fake = run(fake, &events);
        assert_eq!(fake.states, [State::Quitting]);
        assert!(fake.calls.is_empty());
    }

    #[test]
    fn a_vanished_event_source_counts_as_a_quit() {
        let (sender, events) = mpsc::channel::<Event>();
        let fake = Fake::new(mpsc::channel().0).panel(Ok(Panel::Absent));
        drop(sender);
        let fake = run(fake, &events);
        assert_eq!(fake.states, [State::NoDisplay, State::Quitting]);
    }
}
