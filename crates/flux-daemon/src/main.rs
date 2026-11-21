//! `fluxd` — the Oxide Flux engine binary.
//!
//! Everything of substance is in the library; this only forwards the process
//! arguments and exit status.

fn main() -> std::process::ExitCode {
    flux_daemon::cli::main(std::env::args_os())
}
