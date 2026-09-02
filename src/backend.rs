//! What the supervisor needs from Windows: the display, its session, the
//! window's state and signals, console stop requests, and KANALI.

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crate::hold::{hold, HoldEvent, KEEPALIVE_WRITE_RETRIES};
use crate::launcher;
use crate::log::Logger;
use crate::overlapped::UsbprintTransport;
use crate::session::{KeepaliveOutcome, OptionalReply, Session};
use crate::supervisor::{Backend, Hold, Panel};
use crate::usbprint::{self, Device, Discovery};
use crate::watch::{Event, State};
use crate::window::{self, PausedSignal};

/// Console control events turn into a stop request for the holding loop
/// and the watcher.
pub mod stop_signal {
    use crate::win::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static STOP: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn handler(ctrl_type: DWORD) -> BOOL {
        match ctrl_type {
            CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
                STOP.store(true, Ordering::SeqCst);
                crate::window::interrupt();
                crate::window::request_stop();
                1
            }
            _ => 0,
        }
    }

    /// Registers the console handler; false when the process has no
    /// console to register with.
    pub fn install() -> bool {
        unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
    }

    pub fn requested() -> bool {
        STOP.load(Ordering::SeqCst)
    }
}

/// Renders a device string, making an empty one visible.
fn show(value: &str) -> &str {
    if value.is_empty() {
        "(empty)"
    } else {
        value
    }
}

/// Runs the session start on an open device: bootstrap, the activation
/// trigger, and the first Ping. Each step is reported through `report`.
pub fn establish(
    device: Device,
    report: &mut dyn FnMut(&str),
) -> Result<Session<UsbprintTransport>, String> {
    let mut session = Session::new(UsbprintTransport::new(device));
    let started = Instant::now();
    let bootstrap = session
        .bootstrap()
        .map_err(|error| format!("session start failed: {error}"))?;
    report(&format!(
        "session started in {} ms: {} firmware {}",
        started.elapsed().as_millis(),
        show(&bootstrap.device.product_name),
        show(&bootstrap.device.firmware_version)
    ));

    match session.activate() {
        Ok(OptionalReply::Acknowledged) => report("activation trigger acknowledged"),
        Ok(OptionalReply::None) => report("activation trigger sent; no reply within the window"),
        Ok(OptionalReply::Drained) => {
            report("activation trigger sent; an unrelated frame was consumed")
        }
        Err(error) => return Err(format!("activation failed: {error}")),
    }

    match session.ping() {
        KeepaliveOutcome::Sent(OptionalReply::None) => {
            report("ping sent; no reply within the window")
        }
        KeepaliveOutcome::Sent(_) => report("ping sent; a reply was consumed"),
        KeepaliveOutcome::Retryable(error) => report(&format!("ping did not transfer: {error}")),
        KeepaliveOutcome::Fatal(error) => return Err(format!("ping failed: {error}")),
    }
    Ok(session)
}

/// The supervisor's view of this machine.
pub struct WindowsBackend {
    session: Option<Session<UsbprintTransport>>,
    paused_signal: Option<PausedSignal>,
    kanali: Option<PathBuf>,
}

impl WindowsBackend {
    /// `paused_signal` is the confirmation event, when it could be
    /// created; `kanali` is KANALI's executable, when it is installed.
    pub fn new(paused_signal: Option<PausedSignal>, kanali: Option<PathBuf>) -> Self {
        Self {
            session: None,
            paused_signal,
            kanali,
        }
    }
}

impl Backend for WindowsBackend {
    fn stop_requested(&self) -> bool {
        stop_signal::requested()
    }

    fn clear_interrupt(&mut self) {
        window::clear_interrupt();
    }

    fn set_released(&mut self, released: bool) {
        if let Some(signal) = &self.paused_signal {
            if released {
                signal.set();
            } else {
                signal.clear();
            }
        }
    }

    fn report(&mut self, state: State) {
        window::set_state(state);
    }

    fn find_panel(&mut self) -> Result<Panel, String> {
        match usbprint::find_panorama() {
            Ok(Discovery::One(path)) => Ok(Panel::One(path)),
            Ok(Discovery::Absent) => Ok(Panel::Absent),
            Ok(Discovery::Several(paths)) => Ok(Panel::Several(paths.len())),
            Err(error) => Err(error.to_string()),
        }
    }

    fn start_session(&mut self, path: &str, log: &mut Logger) -> Result<(), String> {
        let session = Device::open(path)
            .map_err(|error| error.to_string())
            .and_then(|device| establish(device, &mut |line| log.log(line)))?;
        self.session = Some(session);
        Ok(())
    }

    fn hold_session(&mut self, interval: Duration, verbose: bool, log: &mut Logger) -> Hold {
        let Some(mut session) = self.session.take() else {
            return Hold::Lost {
                pings: 0,
                error: crate::session::SessionError::Closed,
            };
        };
        let last_outbound = session.now();
        let mut pings = 0u64;
        let result = hold(
            &mut session,
            interval,
            last_outbound,
            &|| window::interrupted() || stop_signal::requested(),
            &mut |event| match event {
                HoldEvent::Pinged(_, drained) => {
                    pings += 1;
                    if verbose {
                        log.log(&format!("ping {pings}; {drained} unasked frames drained so far"));
                    }
                }
                HoldEvent::Retrying { attempt, error } => {
                    log.log(&format!("ping retry {attempt} of {KEEPALIVE_WRITE_RETRIES}: {error}"));
                }
                HoldEvent::Stopped => {}
            },
        );
        drop(session);
        match result {
            Ok(()) => Hold::Stopped { pings },
            Err(error) => Hold::Lost { pings, error },
        }
    }

    fn start_kanali(&mut self, log: &mut Logger) -> bool {
        let Some(path) = &self.kanali else {
            log.log("KANALI is not installed");
            return false;
        };
        match launcher::launch(path) {
            Ok(process) => {
                log.log(&format!("started KANALI (process {})", process.id()));
                thread::spawn(move || {
                    process.wait();
                    launcher::wait_for_processes_named(launcher::KANALI_EXE);
                    window::post_event(Event::KanaliClosed);
                });
                true
            }
            Err(error) => {
                log.log(&format!("could not start KANALI: {error}"));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_maps_onto_the_supervisor_view() {
        let mut backend = WindowsBackend::new(None, None);
        match backend.find_panel() {
            Ok(Panel::One(path)) => assert!(usbprint::is_panorama_path(&path)),
            Ok(Panel::Absent) | Ok(Panel::Several(_)) => {}
            Err(error) => panic!("discovery failed: {error}"),
        }
    }

    #[test]
    fn holding_without_a_session_reports_it_closed() {
        let mut backend = WindowsBackend::new(None, None);
        let mut log = Logger::silent();
        assert_eq!(
            backend.hold_session(Duration::from_secs(1), false, &mut log),
            Hold::Lost {
                pings: 0,
                error: crate::session::SessionError::Closed
            }
        );
    }

    #[test]
    fn a_missing_kanali_does_not_start() {
        let mut backend = WindowsBackend::new(None, None);
        let mut log = Logger::silent();
        assert!(!backend.start_kanali(&mut log));
        let mut backend = WindowsBackend::new(None, Some(PathBuf::from(r"C:\ezrama-no-such.exe")));
        assert!(!backend.start_kanali(&mut log));
    }

    #[test]
    fn the_release_flag_drives_the_signal() {
        let name = format!("Local\\ezrama-backend-test-{}", std::process::id());
        let signal = PausedSignal::named(&name).unwrap();
        let mut backend = WindowsBackend::new(Some(signal), None);
        let short = Duration::from_millis(20);
        backend.set_released(true);
        assert_eq!(window::wait_named(&name, short), Ok(true));
        backend.set_released(false);
        assert_eq!(window::wait_named(&name, short), Ok(false));
    }
}
