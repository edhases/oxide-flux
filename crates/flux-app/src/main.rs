// Release builds are GUI-subsystem binaries so Explorer and the installer do
// not open a console window; debug builds keep the console for `cargo run`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> iced::Result {
    // A GUI-subsystem process starts without standard handles, which would
    // swallow `--version`, `--help` and association output when the app is
    // started from a terminal. Reattaching to the parent console restores them
    // and does nothing when there is no parent console.
    flux_app::launch::attach_parent_console();

    match flux_app::launch::prepare(std::env::args_os().skip(1)) {
        Ok(flux_app::Plan::Run(initial)) => flux_app::run(initial),
        Ok(flux_app::Plan::Exit(code)) => std::process::exit(code),
        Err(message) => {
            eprintln!("oxide-flux: {message}");
            eprintln!();
            eprint!("{}", flux_app::launch::USAGE);
            std::process::exit(2);
        }
    }
}
