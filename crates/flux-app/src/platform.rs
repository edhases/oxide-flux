//! Windows shell integration: `.torrent` and `magnet:` associations.
//!
//! Everything is per-user (`HKEY_CURRENT_USER\Software\Classes`), so no
//! administrator rights are needed. Windows 10+ may still show an "Open with"
//! prompt for `.torrent` when a `UserChoice` value already exists; the GUI
//! reports that honestly instead of pretending the association is active.

use std::path::Path;

/// The command line Windows runs for an association.
#[must_use]
pub fn open_command(executable: &Path) -> String {
    format!("\"{}\" \"%1\"", executable.display())
}

/// What the registration actually achieved.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Whether `.torrent` now points at Oxide Flux.
    pub torrent: bool,
    /// The application that kept `.torrent`, when another one owns it.
    pub torrent_owner: Option<String>,
    /// Whether `magnet:` now points at Oxide Flux.
    pub magnet: bool,
}

/// Registers the client for `.torrent` files and `magnet:` links.
///
/// # Errors
///
/// Returns a human-readable message when the executable path or the registry
/// cannot be accessed.
#[cfg(windows)]
pub fn register() -> Result<Report, String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let command = open_command(&executable);
    let icon = format!("\"{}\",0", executable.display());
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    let create = |path: &str| -> Result<RegKey, String> {
        hkcu.create_subkey(path)
            .map(|(key, _)| key)
            .map_err(|error| format!("{path}: {error}"))
    };

    let torrent = create("Software\\Classes\\OxideFlux.Torrent")?;
    torrent
        .set_value("", &"Oxide Flux torrent")
        .map_err(|error| error.to_string())?;
    torrent
        .set_value("FriendlyTypeName", &"Oxide Flux torrent")
        .map_err(|error| error.to_string())?;
    torrent
        .create_subkey("shell\\open\\command")
        .and_then(|(key, _)| key.set_value("", &command))
        .map_err(|error| error.to_string())?;
    torrent
        .create_subkey("DefaultIcon")
        .and_then(|(key, _)| key.set_value("", &icon))
        .map_err(|error| error.to_string())?;

    let magnet = create("Software\\Classes\\OxideFlux.Magnet")?;
    magnet
        .set_value("", &"URL: Magnet link")
        .map_err(|error| error.to_string())?;
    magnet
        .create_subkey("shell\\open\\command")
        .and_then(|(key, _)| key.set_value("", &command))
        .map_err(|error| error.to_string())?;
    magnet
        .create_subkey("DefaultIcon")
        .and_then(|(key, _)| key.set_value("", &icon))
        .map_err(|error| error.to_string())?;

    // The scheme itself must live under `Classes\magnet`; putting
    // `URL Protocol` only on the ProgID does nothing.
    let scheme = create("Software\\Classes\\magnet")?;
    scheme
        .set_value("", &"URL: Magnet link")
        .map_err(|error| error.to_string())?;
    scheme
        .set_value("URL Protocol", &"")
        .map_err(|error| error.to_string())?;
    scheme
        .create_subkey("shell\\open\\command")
        .and_then(|(key, _)| key.set_value("", &command))
        .map_err(|error| error.to_string())?;
    scheme
        .create_subkey("DefaultIcon")
        .and_then(|(key, _)| key.set_value("", &icon))
        .map_err(|error| error.to_string())?;

    let extension = create("Software\\Classes\\.torrent")?;
    let current: Option<String> = extension.get_value("").ok();
    let mut report = Report::default();
    match current.as_deref() {
        None | Some("") | Some("OxideFlux.Torrent") => {
            extension
                .set_value("", &"OxideFlux.Torrent")
                .map_err(|error| error.to_string())?;
            report.torrent = true;
        }
        Some(other) => {
            report.torrent = false;
            report.torrent_owner = Some(other.to_owned());
        }
    }

    let application = create("Software\\Classes\\Applications\\oxide-flux.exe")?;
    application
        .create_subkey("shell\\open\\command")
        .and_then(|(key, _)| key.set_value("", &command))
        .map_err(|error| error.to_string())?;
    application
        .create_subkey("SupportedTypes")
        .and_then(|(key, _)| key.set_value(".torrent", &""))
        .map_err(|error| error.to_string())?;

    let capabilities = create("Software\\OxideFlux\\Capabilities")?;
    capabilities
        .set_value("ApplicationName", &"Oxide Flux")
        .map_err(|error| error.to_string())?;
    capabilities
        .set_value(
            "ApplicationDescription",
            &"Streaming-first BitTorrent client",
        )
        .map_err(|error| error.to_string())?;
    capabilities
        .create_subkey("FileAssociations")
        .and_then(|(key, _)| key.set_value(".torrent", &"OxideFlux.Torrent"))
        .map_err(|error| error.to_string())?;
    capabilities
        .create_subkey("UrlAssociations")
        .and_then(|(key, _)| key.set_value("magnet", &"OxideFlux.Magnet"))
        .map_err(|error| error.to_string())?;
    create("Software\\RegisteredApplications")?
        .set_value("Oxide Flux", &"Software\\OxideFlux\\Capabilities")
        .map_err(|error| error.to_string())?;

    report.magnet = true;
    Ok(report)
}

/// Removes the keys this client created, leaving other applications' choices
/// alone.
///
/// # Errors
///
/// Returns a message when the registry cannot be written.
#[cfg(windows)]
pub fn unregister() -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for path in [
        "Software\\Classes\\OxideFlux.Torrent",
        "Software\\Classes\\OxideFlux.Magnet",
        "Software\\Classes\\Applications\\oxide-flux.exe",
        "Software\\OxideFlux\\Capabilities",
    ] {
        match hkcu.delete_subkey_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{path}: {error}")),
        }
    }

    if let Ok(extension) = hkcu.open_subkey_with_flags(
        "Software\\Classes\\.torrent",
        winreg::enums::KEY_READ | winreg::enums::KEY_WRITE,
    ) {
        let ours: Option<String> = extension.get_value("").ok();
        if ours.as_deref() == Some("OxideFlux.Torrent") {
            let _ = extension.delete_value("");
        }
    }

    // Only remove the magnet scheme if its command still points at this build.
    if let Ok(magnet) = hkcu.open_subkey("Software\\Classes\\magnet\\shell\\open\\command") {
        let command: Option<String> = magnet.get_value("").ok();
        let ours = command.is_some_and(|value| value.contains("oxide-flux"));
        if ours {
            let _ = hkcu.delete_subkey_all("Software\\Classes\\magnet");
        }
    }

    if let Ok(registered) = hkcu.open_subkey_with_flags(
        "Software\\RegisteredApplications",
        winreg::enums::KEY_READ | winreg::enums::KEY_WRITE,
    ) {
        let _ = registered.delete_value("Oxide Flux");
    }
    Ok(())
}

/// Not implemented outside Windows yet.
///
/// # Errors
///
/// Always returns a message on non-Windows targets.
#[cfg(not(windows))]
pub fn register() -> Result<Report, String> {
    Err("file associations are implemented on Windows only".to_owned())
}

/// Not implemented outside Windows yet.
///
/// # Errors
///
/// Always returns a message on non-Windows targets.
#[cfg(not(windows))]
pub fn unregister() -> Result<(), String> {
    Err("file associations are implemented on Windows only".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_command_quotes_the_path_and_placeholder() {
        let command = open_command(Path::new(r"C:\Program Files\Oxide Flux\oxide-flux.exe"));
        assert_eq!(
            command,
            r#""C:\Program Files\Oxide Flux\oxide-flux.exe" "%1""#
        );
    }
}
