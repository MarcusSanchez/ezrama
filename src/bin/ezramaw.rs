//! The same command line as `ezrama`, built without a console so it can run
//! at logon without opening a window.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    ezrama::cli::main()
}
