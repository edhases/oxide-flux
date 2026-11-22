fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Oxide Flux");
        res.set("FileDescription", "Oxide Flux BitTorrent Client");
        if let Err(e) = res.compile() {
            eprintln!("Warning: could not compile Windows resource: {e}");
        }
    }
}
