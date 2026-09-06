//! rssvc - a lightweight Windows service wrapper (NSSM alternative), written in Rust.
//!
//! Single binary, three modes:
//! - launched by SCM as `rssvc.exe <service-name>` -> service runner
//! - launched with a known command -> CLI (install / remove / start / stop / ...)
//! - double-clicked (or `rssvc gui`) -> GUI service manager

mod cli;
mod config;
mod ctl;
mod edit_dlg;
mod gui;
mod install_dlg;
mod log_view;
mod logger;
mod runner;
mod scm;
mod service;
mod util;

use std::sync::OnceLock;

/// Name of the service this process is currently running as (service mode only).
pub static SERVICE_NAME: OnceLock<String> = OnceLock::new();

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // The SCM launches us as: rssvc.exe <service-name>
    // A single argument that is not a known CLI command is treated as a
    // potential service name; if we are not running under the SCM the
    // dispatcher fails with ERROR_FAILED_SERVICE_CONTROLLER_CONNECT and we
    // fall back to CLI mode (this mirrors what nssm does).
    if args.len() == 1 && !cli::is_known_command(&args[0]) {
        if service::try_dispatch(args[0].clone()) {
            return; // ran as a service (or failed hard while doing so)
        }
        // Not a service context -> fall through to CLI, which will print an
        // "unknown command" error for this token.
    }

    // Double-clicked from Explorer (no args, we own the fresh console):
    // open the GUI manager instead of printing help to a console that will
    // close immediately.
    if args.is_empty() && gui::owns_console() {
        gui::launch();
    }

    let code = cli::run(&args);
    std::process::exit(code);
}
