fn main() -> iced::Result {
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
