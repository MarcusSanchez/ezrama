use std::process::ExitCode;

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Usage: ezrama <command> [options]

Commands:
  probe      Find the Panorama SE printer interface and open it briefly
  info       Start a session and print the device's state; changes nothing
  activate   Start a session and switch the panel to its stored media once
  run        Start a session and hold it with keepalive pings until Ctrl+C
  help       Show this message
  version    Print the version

Options:
  -v, --verbose        Extra detail: interfaces and exclusivity for probe,
                       one line per ping for run
  --io                 With probe: arm one read, cancel it, and reap it
  --interval <secs>    With run: seconds between pings (default 4.5)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    let io_check = args.iter().any(|a| a == "--io");
    let interval = interval_argument(&args);
    let command = args
        .iter()
        .enumerate()
        .find(|(index, a)| !a.starts_with('-') && !is_option_value(&args, *index))
        .map(|(_, a)| a.as_str());
    let wants_version = args.iter().any(|a| a == "--version" || a == "-V");
    let wants_help = args.iter().any(|a| a == "--help" || a == "-h");
    match command {
        Some("probe") => probe(verbose, io_check),
        Some("info") => info(),
        Some("activate") => activate(),
        Some("run") => match interval {
            Ok(interval) => run(verbose, interval),
            Err(message) => {
                eprintln!("{message}");
                ExitCode::from(2)
            }
        },
        Some("version") => {
            println!("{NAME} {VERSION}");
            ExitCode::SUCCESS
        }
        Some("help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command: {other}");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
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

/// Exit code when another program holds the device.
const EXIT_BUSY: u8 = 4;
/// Exit code when more than one display is present.
const EXIT_SEVERAL: u8 = 3;

#[cfg(windows)]
mod win_cli {
    use super::{EXIT_BUSY, EXIT_SEVERAL};
    use ezrama::usbprint::{self, Device, Discovery, OpenError};
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
}

#[cfg(windows)]
fn probe(verbose: bool, io_check: bool) -> ExitCode {
    use ezrama::usbprint::{self, Device, OpenError};

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
fn io_probe(device: ezrama::usbprint::Device) -> ExitCode {
    use ezrama::overlapped::{Disarm, UsbprintTransport};
    use ezrama::transport::Transport;
    use std::time::{Duration, Instant};

    let mut transport = UsbprintTransport::new(device);
    let wait = Duration::from_millis(300);
    let started = Instant::now();
    match transport.read(wait) {
        Ok(bytes) if bytes.is_empty() => {
            println!(
                "armed a {} KiB read; nothing arrived within {} ms",
                ezrama::overlapped::READ_BUFFER_SIZE / 1024,
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
    use ezrama::overlapped::UsbprintTransport;
    use ezrama::session::Session;
    use ezrama::wire::{LoopMode, MediaMode, UserConfiguration};
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

/// Opens the display and runs the session start: bootstrap, the activation
/// trigger, and the first Ping. Prints each step.
#[cfg(windows)]
fn start_session() -> Result<ezrama::session::Session<ezrama::overlapped::UsbprintTransport>, ExitCode> {
    use ezrama::overlapped::UsbprintTransport;
    use ezrama::session::{KeepaliveOutcome, OptionalReply, Session};
    use std::time::Instant;

    let path = win_cli::locate()?;
    let device = win_cli::open(&path)?;
    let mut session = Session::new(UsbprintTransport::new(device));

    let started = Instant::now();
    let bootstrap = match session.bootstrap() {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            eprintln!("session start failed: {error}");
            return Err(ExitCode::from(1));
        }
    };
    println!(
        "session started in {} ms: {} firmware {}",
        started.elapsed().as_millis(),
        show(&bootstrap.device.product_name),
        show(&bootstrap.device.firmware_version)
    );

    match session.activate() {
        Ok(OptionalReply::Acknowledged) => println!("activation trigger acknowledged"),
        Ok(OptionalReply::None) => println!("activation trigger sent; no reply within the window"),
        Ok(OptionalReply::Drained) => println!("activation trigger sent; an unrelated frame was consumed"),
        Err(error) => {
            eprintln!("activation failed: {error}");
            return Err(ExitCode::from(1));
        }
    }

    match session.ping() {
        KeepaliveOutcome::Sent(OptionalReply::None) => println!("ping sent; no reply within the window"),
        KeepaliveOutcome::Sent(_) => println!("ping sent; a reply was consumed"),
        KeepaliveOutcome::Retryable(error) => println!("ping did not transfer: {error}"),
        KeepaliveOutcome::Fatal(error) => {
            eprintln!("ping failed: {error}");
            return Err(ExitCode::from(1));
        }
    }
    Ok(session)
}

/// Starts a session, reports which media the panel is configured to show,
/// and exits without holding the session.
#[cfg(windows)]
fn activate() -> ExitCode {
    let mut session = match start_session() {
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

/// Console control events turn into a stop request for the holding loop.
#[cfg(windows)]
mod stop_signal {
    use ezrama::win::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static STOP: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn handler(ctrl_type: DWORD) -> BOOL {
        match ctrl_type {
            CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
                STOP.store(true, Ordering::SeqCst);
                1
            }
            _ => 0,
        }
    }

    pub fn install() -> bool {
        unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
    }

    pub fn requested() -> bool {
        STOP.load(Ordering::SeqCst)
    }
}

/// Starts a session and holds it with periodic Pings until Ctrl+C or loss.
#[cfg(windows)]
fn run(verbose: bool, interval: std::time::Duration) -> ExitCode {
    use ezrama::hold::{hold, HoldEvent, KEEPALIVE_WRITE_RETRIES};
    use ezrama::session::OptionalReply;

    let mut session = match start_session() {
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
        &stop_signal::requested,
        &mut |event| match event {
            HoldEvent::Pinged(reply) => {
                pings += 1;
                if verbose {
                    let reply = match reply {
                        OptionalReply::None => "no reply",
                        OptionalReply::Acknowledged => "acknowledged",
                        OptionalReply::Drained => "reply consumed",
                    };
                    println!("ping {pings}: {reply}");
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
        Err(error) => {
            eprintln!("session lost after {pings} pings: {error}");
            ExitCode::from(1)
        }
    }
}

/// Parses `--interval <seconds>`; the built-in interval when absent.
fn interval_argument(args: &[String]) -> Result<std::time::Duration, String> {
    use std::time::Duration;

    let Some(position) = args.iter().position(|a| a == "--interval") else {
        return Ok(ezrama::hold::KEEPALIVE_INTERVAL);
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

#[cfg(not(windows))]
fn probe(_verbose: bool, _io_check: bool) -> ExitCode {
    eprintln!("probe is only available on Windows");
    ExitCode::from(1)
}

#[cfg(not(windows))]
fn info() -> ExitCode {
    eprintln!("info is only available on Windows");
    ExitCode::from(1)
}

#[cfg(not(windows))]
fn activate() -> ExitCode {
    eprintln!("activate is only available on Windows");
    ExitCode::from(1)
}

#[cfg(not(windows))]
fn run(_verbose: bool, _interval: std::time::Duration) -> ExitCode {
    eprintln!("run is only available on Windows");
    ExitCode::from(1)
}
