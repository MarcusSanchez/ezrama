//! The command line shared by the console and windowless binaries.

use std::process::ExitCode;
use std::time::Duration;

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Usage: ezrama <command> [options]

Commands:
  probe      Find the Panorama SE printer interface and open it briefly
  info       Start a session and print the device's state; changes nothing
  activate   Start a session and switch the panel to its stored media once
  run        Start a session and hold it with keepalive pings until Ctrl+C
  watch      Hold a session whenever the panel is present, with a tray icon
  pause      Ask the running watcher to release the panel and wait for it
  resume     Ask the running watcher to take the panel back
  kanali     Ask the running watcher to release the panel, start KANALI, and
             take the panel back once KANALI exits
  stop       Ask the running watcher to exit
  install    Copy the binaries to local app data, start the watcher at logon
  uninstall  Stop the watcher and remove the logon entry and the binaries
  status     Report the installation, the watcher, and the panel
  help       Show this message
  version    Print the version

Options:
  -v, --verbose        Extra detail: interfaces and exclusivity for probe,
                       one line per ping for run and watch
  --io                 With probe: arm one read, cancel it, and reap it
  --interval <secs>    With run and watch: seconds between pings (default 4.5)
";

/// Exit code when another program holds the device.
const EXIT_BUSY: u8 = 4;
/// Exit code when more than one display is present.
const EXIT_SEVERAL: u8 = 3;
/// Exit code when `watch` finds a watcher already running.
const EXIT_ALREADY_RUNNING: u8 = 5;
/// How long `pause` waits for the watcher to confirm the release.
const PAUSE_CONFIRMATION: Duration = Duration::from_secs(5);
/// How long `install` and `uninstall` wait for a stopped watcher to exit;
/// a session start attempt can hold it for the readiness deadline.
const STOP_CONFIRMATION: Duration = Duration::from_secs(30);

/// A request for a running watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatcherRequest {
    Pause,
    Resume,
    OpenKanali,
    Stop,
}

/// Runs the command line given by the process arguments.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    dispatch(&args)
}

fn dispatch(args: &[String]) -> ExitCode {
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    let io_check = args.iter().any(|a| a == "--io");
    let interval = interval_argument(args);
    let command = args
        .iter()
        .enumerate()
        .find(|(index, a)| !a.starts_with('-') && !is_option_value(args, *index))
        .map(|(_, a)| a.as_str());
    let wants_version = args.iter().any(|a| a == "--version" || a == "-V");
    let wants_help = args.iter().any(|a| a == "--help" || a == "-h");
    match command {
        Some("probe") => probe(verbose, io_check),
        Some("info") => info(),
        Some("activate") => activate(),
        Some("run") => match interval {
            Ok(interval) => run(verbose, interval),
            Err(message) => usage_error(&message),
        },
        Some("watch") => match interval {
            Ok(interval) => watch(verbose, interval),
            Err(message) => usage_error(&message),
        },
        Some("pause") => control(WatcherRequest::Pause),
        Some("resume") => control(WatcherRequest::Resume),
        Some("kanali") => control(WatcherRequest::OpenKanali),
        Some("stop") => control(WatcherRequest::Stop),
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some("status") => status(),
        Some("version") => {
            println!("{NAME} {VERSION}");
            ExitCode::SUCCESS
        }
        Some("help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => usage_error(&format!("unknown command: {other}")),
        None if wants_version => {
            println!("{NAME} {VERSION}");
            ExitCode::SUCCESS
        }
        None if wants_help => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        None => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("{message}");
    eprint!("{USAGE}");
    ExitCode::from(2)
}

/// Parses `--interval <seconds>`; the built-in interval when absent.
fn interval_argument(args: &[String]) -> Result<Duration, String> {
    let Some(position) = args.iter().position(|a| a == "--interval") else {
        return Ok(crate::hold::KEEPALIVE_INTERVAL);
    };
    let Some(value) = args.get(position + 1) else {
        return Err("--interval needs a value in seconds".to_string());
    };
    match value.parse::<f64>() {
        Ok(seconds) if (0.1..=3600.0).contains(&seconds) => Ok(Duration::from_secs_f64(seconds)),
        _ => Err(format!("--interval must be between 0.1 and 3600 seconds, not {value}")),
    }
}

/// Whether the argument at `index` is the value of a preceding option.
fn is_option_value(args: &[String], index: usize) -> bool {
    index > 0 && args[index - 1] == "--interval"
}

/// Renders a device string, making an empty one visible.
fn show(value: &str) -> &str {
    if value.is_empty() {
        "(empty)"
    } else {
        value
    }
}

#[cfg(windows)]
mod win_cli {
    use super::{EXIT_BUSY, EXIT_SEVERAL};
    use crate::overlapped::UsbprintTransport;
    use crate::session::Session;
    use crate::usbprint::{self, Device, Discovery, OpenError};
    use std::process::ExitCode;

    /// Finds the one display, printing why when that is not possible.
    pub fn locate() -> Result<String, ExitCode> {
        match usbprint::find_panorama() {
            Ok(Discovery::One(path)) => {
                println!("Panorama SE: {path}");
                Ok(path)
            }
            Ok(Discovery::Absent) => {
                eprintln!("no Panorama SE printer interface is present");
                Err(ExitCode::from(1))
            }
            Ok(Discovery::Several(paths)) => {
                eprintln!("{} Panorama SE printer interfaces are present:", paths.len());
                for path in &paths {
                    eprintln!("  {path}");
                }
                Err(ExitCode::from(EXIT_SEVERAL))
            }
            Err(error) => {
                eprintln!("discovery failed: {error}");
                Err(ExitCode::from(1))
            }
        }
    }

    /// Opens the display, printing why when that is not possible.
    pub fn open(path: &str) -> Result<Device, ExitCode> {
        match Device::open(path) {
            Ok(device) => Ok(device),
            Err(error @ OpenError::Busy(_)) => {
                eprintln!("{error}");
                Err(ExitCode::from(EXIT_BUSY))
            }
            Err(error) => {
                eprintln!("{error}");
                Err(ExitCode::from(1))
            }
        }
    }

    /// Locates and opens the display, then runs the session start, printing
    /// each step.
    pub fn start_session() -> Result<Session<UsbprintTransport>, ExitCode> {
        let path = locate()?;
        let device = open(&path)?;
        crate::backend::establish(device, &mut |line| println!("{line}")).map_err(|message| {
            eprintln!("{message}");
            ExitCode::from(1)
        })
    }
}

#[cfg(windows)]
fn probe(verbose: bool, io_check: bool) -> ExitCode {
    use crate::usbprint::{self, Device, OpenError};

    if verbose {
        match usbprint::printer_interfaces() {
            Ok(paths) => {
                println!("printer-class interfaces present: {}", paths.len());
                for path in &paths {
                    println!("  {path}");
                }
            }
            Err(error) => {
                eprintln!("probe failed: {error}");
                return ExitCode::from(1);
            }
        }
    }

    let path = match win_cli::locate() {
        Ok(path) => path,
        Err(code) => return code,
    };
    let device = match win_cli::open(&path) {
        Ok(device) => device,
        Err(code) => return code,
    };
    println!("opened for exclusive use");
    if verbose {
        match Device::open(&path) {
            Err(OpenError::Busy(error)) => {
                println!("second open refused while held: {error}");
            }
            Err(error) => println!("second open failed: {error}"),
            Ok(_) => println!("second open succeeded; the driver does not enforce exclusivity"),
        }
    }
    if io_check {
        let status = io_probe(device);
        println!("closed");
        return status;
    }
    drop(device);
    println!("closed");
    ExitCode::SUCCESS
}

/// Arms one input transfer, lets it time out, cancels it, and reaps it.
/// Nothing is written to the device.
#[cfg(windows)]
fn io_probe(device: crate::usbprint::Device) -> ExitCode {
    use crate::overlapped::{Disarm, UsbprintTransport};
    use crate::transport::Transport;
    use std::time::Instant;

    let mut transport = UsbprintTransport::new(device);
    let wait = Duration::from_millis(300);
    let started = Instant::now();
    match transport.read(wait) {
        Ok(bytes) if bytes.is_empty() => {
            println!(
                "armed a {} KiB read; nothing arrived within {} ms",
                crate::overlapped::READ_BUFFER_SIZE / 1024,
                started.elapsed().as_millis()
            );
        }
        Ok(bytes) => {
            println!("armed a read; {} unsolicited bytes arrived", bytes.len());
        }
        Err(error) => {
            eprintln!("read failed: {error}");
            return ExitCode::from(1);
        }
    }
    println!("read still armed: {}", transport.read_armed());
    match transport.disarm_read() {
        Disarm::Cancelled(elapsed) => {
            println!("cancelled and reaped in {} ms", elapsed.as_millis());
        }
        Disarm::Completed(n) => println!("read had completed with {n} bytes before cancel"),
        Disarm::NotArmed => println!("no read was armed"),
        Disarm::Abandoned => {
            eprintln!("cancellation did not complete; buffer abandoned");
            return ExitCode::from(1);
        }
    }
    println!("read still armed: {}", transport.read_armed());
    match transport.lost() {
        None => ExitCode::SUCCESS,
        Some(reason) => {
            eprintln!("transport lost: {reason}");
            ExitCode::from(1)
        }
    }
}

/// Starts a session, reads the device's information and configuration, and
/// prints them. Sends only the read-only bootstrap and configuration queries.
#[cfg(windows)]
fn info() -> ExitCode {
    use crate::overlapped::UsbprintTransport;
    use crate::session::Session;
    use crate::wire::{LoopMode, MediaMode, UserConfiguration};
    use std::time::Instant;

    let path = match win_cli::locate() {
        Ok(path) => path,
        Err(code) => return code,
    };
    let device = match win_cli::open(&path) {
        Ok(device) => device,
        Err(code) => return code,
    };
    let mut session = Session::new(UsbprintTransport::new(device));

    let started = Instant::now();
    let bootstrap = match session.bootstrap() {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            eprintln!("session start failed: {error}");
            return ExitCode::from(1);
        }
    };
    let elapsed = started.elapsed().as_millis();
    let device = &bootstrap.device;
    println!("session started in {elapsed} ms after {} DeviceInfo attempt(s)", bootstrap.readiness_attempts);
    println!("Device");
    println!("  product:    {}", show(&device.product_name));
    println!("  os:         {} {}", show(&device.os_name), device.os_version);
    println!("  firmware:   {}", show(&device.firmware_version));
    println!("  app:        {}", show(&device.app_version));
    println!(
        "  serial:     {}{}",
        show(&device.serial_number),
        if device.serial_number_locked { " (locked)" } else { "" }
    );
    println!("  chip id:    {}", show(&device.chip_id));
    println!("  auth:       {}", show(&bootstrap.auth));

    let config: UserConfiguration = match session.query_user_configuration() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration query failed: {error}");
            return ExitCode::from(1);
        }
    };
    println!("Configuration");
    match &config.poweron {
        Some(poweron) => println!("  power-on:   {}", show(&poweron.media_file)),
        None => println!("  power-on:   not reported"),
    }
    match &config.standby {
        Some(standby) => println!(
            "  standby:    {}, media {}",
            if standby.enable { "enabled" } else { "disabled" },
            show(&standby.media_file)
        ),
        None => println!("  standby:    not reported"),
    }
    match &config.work {
        Some(work) => {
            let mode = match work.media_mode {
                MediaMode::Single => "single".to_string(),
                MediaMode::Dual => "dual".to_string(),
                MediaMode::Kaleidoscope => "kaleidoscope".to_string(),
                MediaMode::Unknown(value) => format!("unknown ({value})"),
            };
            let looping = match work.loop_mode {
                LoopMode::Single => "single".to_string(),
                LoopMode::All => "all".to_string(),
                LoopMode::Random => "random".to_string(),
                LoopMode::Unknown(value) => format!("unknown ({value})"),
            };
            println!("  work mode:  {mode}, loop {looping}");
            println!("  single:     {}", show(&work.single_mode_media_file));
            println!(
                "  dual:       left {}, right {}",
                show(&work.dual_mode_left_media_file),
                show(&work.dual_mode_right_media_file)
            );
            println!(
                "  kaleido:    {} (source {})",
                show(&work.kaleidoscope_media_file),
                work.kaleidoscope_source
            );
        }
        None => println!("  work:       not reported"),
    }
    match &config.display {
        Some(display) => {
            println!(
                "  backlight:  {}, brightness {}",
                if display.backlight_enable { "on" } else { "off" },
                display.backlight_brightness
            );
            println!(
                "  rotation:   ui {}, media {}, mirror flag {}",
                display.ui_rotation, display.media_rotation, display.mirror
            );
        }
        None => println!("  display:    not reported"),
    }
    match session.closed_by() {
        None => ExitCode::SUCCESS,
        Some(error) => {
            eprintln!("session closed: {error}");
            ExitCode::from(1)
        }
    }
}

/// Starts a session, reports which media the panel is configured to show,
/// and exits without holding the session.
#[cfg(windows)]
fn activate() -> ExitCode {
    let mut session = match win_cli::start_session() {
        Ok(session) => session,
        Err(code) => return code,
    };
    match session.query_user_configuration() {
        Ok(config) => match config.work {
            Some(work) => println!("panel is configured to show {}", show(&work.single_mode_media_file)),
            None => println!("panel reported no work configuration"),
        },
        Err(error) => {
            eprintln!("configuration query failed: {error}");
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

/// Starts a session and holds it with periodic Pings until Ctrl+C or loss.
#[cfg(windows)]
fn run(verbose: bool, interval: Duration) -> ExitCode {
    use crate::backend::stop_signal;
    use crate::window;
    use crate::hold::{hold, HoldEvent, KEEPALIVE_WRITE_RETRIES};
    use crate::session::{OptionalReply, SessionError};
    use crate::transport::ReadError;

    let mut session = match win_cli::start_session() {
        Ok(session) => session,
        Err(code) => return code,
    };
    if !stop_signal::install() {
        eprintln!("could not install the console stop handler; use the task manager to stop");
    }
    println!(
        "holding the session: ping every {} ms; press Ctrl+C to stop",
        interval.as_millis()
    );

    let last_outbound = session.now();
    let mut pings = 0u64;
    let result = hold(
        &mut session,
        interval,
        last_outbound,
        &mut |remaining| {
            window::wait_interrupt(remaining);
            stop_signal::requested()
        },
        &mut |event| match event {
            HoldEvent::Pinged { reply, drained, gap } => {
                pings += 1;
                if verbose {
                    let reply = match reply {
                        OptionalReply::None => "no reply",
                        OptionalReply::Acknowledged => "acknowledged",
                        OptionalReply::Drained => "reply consumed",
                    };
                    println!(
                        "ping {pings}: {reply}; {} ms since the last; {drained} unasked frames drained so far",
                        gap.as_millis()
                    );
                }
            }
            HoldEvent::Retrying { attempt, error } => {
                println!("ping retry {attempt} of {KEEPALIVE_WRITE_RETRIES}: {error}");
            }
            HoldEvent::Stopped => println!("stop requested after {pings} pings"),
        },
    );
    match result {
        Ok(()) => {
            println!("session released");
            ExitCode::SUCCESS
        }
        Err(SessionError::Read(ReadError::Interrupted)) => {
            println!("stop requested after {pings} pings; session released");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("session lost after {pings} pings: {error}");
            ExitCode::from(1)
        }
    }
}

/// Keeps a session whenever the display is present, with a
/// notification-area icon and a log file. The policy lives in
/// [`crate::supervisor`]; this sets the machine up and tears it down.
#[cfg(windows)]
fn watch(verbose: bool, interval: Duration) -> ExitCode {
    use crate::backend::{stop_signal, WindowsBackend};
    use crate::launcher;
    use crate::log::{default_log_path, Logger};
    use crate::supervisor::Supervisor;
    use crate::tray;
    use crate::watch::Event;
    use crate::window::{self, PausedSignal, Setup};
    use std::sync::mpsc;
    use std::thread;

    let mut log = match default_log_path() {
        Some(path) => Logger::to_file(&path, true),
        None => Logger::stdout(),
    };
    if let Some(id) = window::watcher_process_id() {
        log.log(&format!("a watcher is already running as process {id}; nothing to do"));
        return ExitCode::from(EXIT_ALREADY_RUNNING);
    }
    log.log(&format!(
        "watch starting: {NAME} {VERSION}, ping interval {} ms",
        interval.as_millis()
    ));
    if !stop_signal::install() {
        log.log("console stop handler unavailable");
    }
    let paused_signal = match PausedSignal::create() {
        Ok(signal) => Some(signal),
        Err(error) => {
            log.log(&format!("pause confirmation unavailable: {error}"));
            None
        }
    };

    tray::enable_dpi_awareness();
    let kanali = launcher::kanali_path();
    match &kanali {
        Some(path) => log.log(&format!("KANALI found at {}", path.display())),
        None => log.log("KANALI not found; the menu cannot start it"),
    }
    let icon = match tray::app_icon() {
        Ok(icon) => Some(icon),
        Err(error) => {
            log.log(&format!("notification icon unavailable: {error}"));
            None
        }
    };
    let setup = Setup {
        icon,
        kanali_available: kanali.is_some(),
    };

    let (sender, events) = mpsc::channel::<Event>();
    let pump = thread::spawn(move || window::run_message_loop(sender, setup));

    let backend = WindowsBackend::new(paused_signal, kanali);
    let (_, mut log) = Supervisor::new(backend, log, interval, verbose).run(&events);

    window::request_stop();
    match pump.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => log.log(&format!("device notifications failed: {error}")),
        Err(_) => log.log("device notification thread failed"),
    }
    ExitCode::SUCCESS
}

/// Sends a request to the watcher running in this desktop session. For a
/// pause, waits for the watcher to confirm it has released the panel.
#[cfg(windows)]
fn control(request: WatcherRequest) -> ExitCode {
    use crate::window::{self, Request};

    let sent = match request {
        WatcherRequest::Pause => window::control_watcher(Request::Pause),
        WatcherRequest::Resume => window::control_watcher(Request::Resume),
        WatcherRequest::OpenKanali => window::control_watcher(Request::OpenKanali),
        WatcherRequest::Stop => window::control_watcher(Request::Stop),
    };
    if !sent {
        eprintln!("no ezrama watcher is running");
        return ExitCode::from(1);
    }
    match request {
        WatcherRequest::Pause => match window::wait_paused(PAUSE_CONFIRMATION) {
            Ok(true) => {
                println!("watcher paused; the panel is released");
                ExitCode::SUCCESS
            }
            Ok(false) => {
                eprintln!(
                    "watcher did not confirm the pause within {} s",
                    PAUSE_CONFIRMATION.as_secs()
                );
                ExitCode::from(1)
            }
            Err(error) => {
                eprintln!("pause requested, but there is no confirmation to wait for: {error}");
                ExitCode::from(1)
            }
        },
        WatcherRequest::Resume => {
            println!("watcher resumed");
            ExitCode::SUCCESS
        }
        WatcherRequest::OpenKanali => {
            println!("watcher asked to start KANALI");
            ExitCode::SUCCESS
        }
        WatcherRequest::Stop => {
            println!("watcher asked to stop");
            ExitCode::SUCCESS
        }
    }
}

#[cfg(not(windows))]
fn probe(_verbose: bool, _io_check: bool) -> ExitCode {
    unsupported("probe")
}

#[cfg(not(windows))]
fn info() -> ExitCode {
    unsupported("info")
}

#[cfg(not(windows))]
fn activate() -> ExitCode {
    unsupported("activate")
}

#[cfg(not(windows))]
fn run(_verbose: bool, _interval: Duration) -> ExitCode {
    unsupported("run")
}

#[cfg(not(windows))]
fn watch(_verbose: bool, _interval: Duration) -> ExitCode {
    unsupported("watch")
}

#[cfg(not(windows))]
fn control(_request: WatcherRequest) -> ExitCode {
    unsupported("watcher control")
}

/// Asks a running watcher to stop and waits for its process to exit, which
/// can take a while when it is inside a session start attempt. Returns
/// whether one was running.
#[cfg(windows)]
fn stop_watcher_and_wait() -> bool {
    use crate::launcher::Process;
    use crate::window::{self, Request};
    use std::time::Instant;

    let Some(id) = window::watcher_process_id() else {
        return false;
    };
    let process = Process::open(id).ok();
    window::control_watcher(Request::Stop);
    match process {
        Some(process) => {
            if !process.wait_for(STOP_CONFIRMATION) {
                eprintln!(
                    "the watcher has not exited within {} s",
                    STOP_CONFIRMATION.as_secs()
                );
            }
        }
        None => {
            let deadline = Instant::now() + STOP_CONFIRMATION;
            while window::find_watcher().is_some() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    true
}

/// Starts the windowless watcher at `watcher`, detached from this process.
#[cfg(windows)]
fn spawn_watcher(watcher: &std::path::Path) -> Result<u32, crate::usbprint::WinError> {
    crate::launcher::start_detached(watcher, "watch", None).map(|process| process.id())
}

/// Copies the binaries next to the log, registers the logon entry, and
/// starts the installed watcher.
#[cfg(windows)]
fn install() -> ExitCode {
    use crate::install::*;
    use crate::shortcut;

    let Some(directory) = install_dir() else {
        eprintln!("LOCALAPPDATA is not set; cannot choose an installation directory");
        return ExitCode::from(1);
    };
    let source_dir = match std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        Some(dir) => dir,
        None => {
            eprintln!("cannot locate this program's own directory");
            return ExitCode::from(1);
        }
    };
    let installed_console = directory.join(CONSOLE_BINARY);
    let installed_watcher = directory.join(WATCHER_BINARY);

    if stop_watcher_and_wait() {
        println!("stopped the running watcher");
    }

    if source_dir != directory {
        for name in [CONSOLE_BINARY, WATCHER_BINARY] {
            let source = source_dir.join(name);
            let destination = directory.join(name);
            match copy_binary(&source, &destination) {
                Ok(bytes) => println!("copied {name} to {} ({bytes} bytes)", directory.display()),
                Err(error) => {
                    eprintln!("cannot copy {} to {}: {error}", source.display(), destination.display());
                    return ExitCode::from(1);
                }
            }
        }
    } else {
        println!("already running from {}", directory.display());
    }
    let icon = match write_icon(&directory) {
        Ok(path) => {
            println!("wrote {}", path.display());
            path
        }
        Err(error) => {
            eprintln!("cannot write the icon file: {error}");
            return ExitCode::from(1);
        }
    };
    match shortcut::start_menu_path() {
        Some(link) => {
            let written = shortcut::write(
                &link,
                &installed_watcher,
                "watch",
                &icon,
                "Keeps the Panorama SE showing its media",
            );
            match written {
                Ok(()) => println!("Start Menu entry {}", link.display()),
                Err(error) => {
                    eprintln!("cannot write the Start Menu entry: {error}");
                    return ExitCode::from(1);
                }
            }
        }
        None => println!("APPDATA is not set; no Start Menu entry"),
    }

    let command = run_command(&installed_watcher);
    if let Err(error) = set_run_value(RUN_VALUE, &command) {
        eprintln!("cannot register the logon entry: {error}");
        return ExitCode::from(1);
    }
    println!("logon entry {RUN_VALUE}: {command}");

    match kanali_run_entries() {
        Ok(entries) if !entries.is_empty() => {
            for entry in &entries {
                println!(
                    "note: a KANALI startup entry named {} is still enabled: {}",
                    entry.name, entry.command
                );
            }
            println!("      turn off start with Windows in KANALI's settings, or the two will race for the panel at logon");
        }
        Ok(_) => {}
        Err(error) => println!("could not check for KANALI startup entries: {error}"),
    }

    match spawn_watcher(&installed_watcher) {
        Ok(pid) => println!("started the installed watcher (process {pid})"),
        Err(error) => {
            eprintln!("installed, but could not start the watcher now: {error}");
            return ExitCode::from(1);
        }
    }
    let _ = installed_console;
    ExitCode::SUCCESS
}

/// Stops the watcher, removes the logon entry, and deletes the installed
/// binaries. The log file is left in place.
#[cfg(windows)]
fn uninstall() -> ExitCode {
    use crate::install::*;
    use crate::log::default_log_path;
    use crate::shortcut;

    if stop_watcher_and_wait() {
        println!("stopped the running watcher");
    }
    match delete_run_value(RUN_VALUE) {
        Ok(true) => println!("removed the logon entry"),
        Ok(false) => println!("no logon entry was registered"),
        Err(error) => {
            eprintln!("cannot remove the logon entry: {error}");
            return ExitCode::from(1);
        }
    }
    let Some(directory) = install_dir() else {
        eprintln!("LOCALAPPDATA is not set; nothing to delete");
        return ExitCode::from(1);
    };
    let mut failed = false;
    if let Some(link) = shortcut::start_menu_path() {
        match std::fs::remove_file(&link) {
            Ok(()) => println!("removed the Start Menu entry"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("cannot remove {}: {error}", link.display());
                failed = true;
            }
        }
    }
    for name in [CONSOLE_BINARY, WATCHER_BINARY, ICON_FILE] {
        let path = directory.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => println!("deleted {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("cannot delete {}: {error}", path.display());
                failed = true;
            }
        }
    }
    if let Some(log) = default_log_path() {
        if log.exists() {
            println!("log kept at {}", log.display());
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Reports the installation, the watcher, and the panel.
#[cfg(windows)]
fn status() -> ExitCode {
    use crate::window;
    use crate::install::*;
    use crate::shortcut;
    use crate::usbprint::{self, Discovery};

    match install_dir() {
        Some(directory) => {
            for name in [CONSOLE_BINARY, WATCHER_BINARY] {
                let path = directory.join(name);
                println!(
                    "{name:<12} {}",
                    if path.exists() { format!("installed at {}", path.display()) } else { "not installed".to_string() }
                );
            }
        }
        None => println!("install dir  unknown (LOCALAPPDATA is not set)"),
    }
    match read_run_value(RUN_VALUE) {
        Ok(Some(command)) => println!("logon entry  {command}"),
        Ok(None) => println!("logon entry  none"),
        Err(error) => println!("logon entry  unreadable: {error}"),
    }
    match shortcut::start_menu_path() {
        Some(link) if link.exists() => println!("start menu   {}", link.display()),
        _ => println!("start menu   none"),
    }
    match kanali_run_entries() {
        Ok(entries) if entries.is_empty() => println!("kanali       no startup entry"),
        Ok(entries) => {
            for entry in entries {
                println!("kanali       startup entry {}: {}", entry.name, entry.command);
            }
        }
        Err(error) => println!("kanali       unreadable: {error}"),
    }
    let watcher = match window::find_watcher() {
        None => "not running".to_string(),
        Some(_) => match window::query_watcher_state() {
            Some(state) => format!("running: {}", state.label()),
            None => "running, not answering".to_string(),
        },
    };
    println!("watcher      {watcher}");
    match usbprint::find_panorama() {
        Ok(Discovery::One(path)) => println!("panel        present at {path}"),
        Ok(Discovery::Absent) => println!("panel        not present"),
        Ok(Discovery::Several(paths)) => println!("panel        {} present", paths.len()),
        Err(error) => println!("panel        discovery failed: {error}"),
    }
    ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn install() -> ExitCode {
    unsupported("install")
}

#[cfg(not(windows))]
fn uninstall() -> ExitCode {
    unsupported("uninstall")
}

#[cfg(not(windows))]
fn status() -> ExitCode {
    unsupported("status")
}

#[cfg(not(windows))]
fn unsupported(command: &str) -> ExitCode {
    eprintln!("{command} is only available on Windows");
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn interval_defaults_and_parses() {
        assert_eq!(interval_argument(&args(&["run"])), Ok(crate::hold::KEEPALIVE_INTERVAL));
        assert_eq!(
            interval_argument(&args(&["run", "--interval", "2"])),
            Ok(Duration::from_secs(2))
        );
        assert_eq!(
            interval_argument(&args(&["--interval", "0.5", "run"])),
            Ok(Duration::from_millis(500))
        );
        assert!(interval_argument(&args(&["run", "--interval"])).is_err());
        assert!(interval_argument(&args(&["run", "--interval", "0"])).is_err());
        assert!(interval_argument(&args(&["run", "--interval", "abc"])).is_err());
    }

    #[test]
    fn option_values_are_not_commands() {
        let list = args(&["--interval", "4", "watch"]);
        assert!(is_option_value(&list, 1));
        assert!(!is_option_value(&list, 2));
        assert!(!is_option_value(&list, 0));
    }

    #[test]
    fn show_marks_empty_strings() {
        assert_eq!(show(""), "(empty)");
        assert_eq!(show("PASE"), "PASE");
    }
}
