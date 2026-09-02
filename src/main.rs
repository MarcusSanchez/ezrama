use std::process::ExitCode;

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Usage: ezrama <command> [options]

Commands:
  probe      Find the Panorama SE printer interface and open it briefly
  help       Show this message
  version    Print the version

Options:
  -v, --verbose    Show every printer-class interface and check exclusivity
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    let command = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    let wants_version = args.iter().any(|a| a == "--version" || a == "-V");
    let wants_help = args.iter().any(|a| a == "--help" || a == "-h");
    match command {
        Some("probe") => probe(verbose),
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

#[cfg(windows)]
fn probe(verbose: bool) -> ExitCode {
    use ezrama::usbprint::{self, Device, Discovery, OpenError};

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

    match usbprint::find_panorama() {
        Ok(Discovery::One(path)) => {
            println!("Panorama SE: {path}");
            let device = match Device::open(&path) {
                Ok(device) => device,
                Err(error @ OpenError::Busy(_)) => {
                    eprintln!("{error}");
                    return ExitCode::from(4);
                }
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::from(1);
                }
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
            drop(device);
            println!("closed");
            ExitCode::SUCCESS
        }
        Ok(Discovery::Absent) => {
            eprintln!("no Panorama SE printer interface is present");
            ExitCode::from(1)
        }
        Ok(Discovery::Several(paths)) => {
            eprintln!("{} Panorama SE printer interfaces are present:", paths.len());
            for path in &paths {
                eprintln!("  {path}");
            }
            ExitCode::from(3)
        }
        Err(error) => {
            eprintln!("probe failed: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn probe(_verbose: bool) -> ExitCode {
    eprintln!("probe is only available on Windows");
    ExitCode::from(1)
}
