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
    // Any single-argument launch may be a service start, so try the service
    // dispatcher FIRST — even when the name collides with a CLI command (a
    // service literally named "gui" or "start" is perfectly legal and must
    // be startable). In a console context StartServiceCtrlDispatcher fails
    // fast with ERROR_FAILED_SERVICE_CONTROLLER_CONNECT and we fall through
    // to the CLI, which mirrors what nssm does.
    if args.len() == 1 && service::try_dispatch(args[0].clone()) {
        return; // ran as a service (or failed hard while doing so)
    }
    // Not a service context -> fall through to CLI, which will print an
    // "unknown command" error / usage for this token.

    // Double-clicked from Explorer (no args, we own the fresh console):
    // open the GUI manager instead of printing help to a console that will
    // close immediately.
    if args.is_empty() && gui::owns_console() {
        gui::launch();
    }

    let code = cli::run(&args);
    std::process::exit(code);
}
