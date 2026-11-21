//! Cross-platform validation of file paths taken from torrent metadata.
//!
//! Paths in a `.torrent` are attacker-controlled. The rules enforced here are
//! the Windows rules, applied on every platform, because a download directory
//! may later be moved to or synchronised with a Windows host. Anything that
//! Windows would interpret differently is rejected rather than rewritten.
//!
//! The result is a *relative* [`PathBuf`] that is safe to join under the
//! download root. Callers must still ensure the joined path stays under that
//! root, but every component returned by [`sanitize`] is a plain file name.

use std::path::PathBuf;

/// Maximum number of bytes in one path component.
pub const MAX_COMPONENT_BYTES: usize = 255;

/// Maximum number of bytes in a complete relative path.
pub const MAX_PATH_BYTES: usize = 4096;

/// A value that can be treated as one path component.
///
/// Implemented for `str`, `String`, `[u8]` and `Vec<u8>` so that callers can
/// pass bencoded byte paths directly without converting to UTF-8 first.
pub trait PathComponent {
    /// Borrow the raw component bytes.
    fn component_bytes(&self) -> &[u8];
}

impl PathComponent for str {
    fn component_bytes(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl PathComponent for String {
    fn component_bytes(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl PathComponent for [u8] {
    fn component_bytes(&self) -> &[u8] {
        self
    }
}

impl PathComponent for Vec<u8> {
    fn component_bytes(&self) -> &[u8] {
        self
    }
}

impl<T: PathComponent + ?Sized> PathComponent for &T {
    fn component_bytes(&self) -> &[u8] {
        (**self).component_bytes()
    }
}

/// Why a path was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// No components were supplied.
    #[error("path has no components")]
    EmptyPath,
    /// A component was empty.
    #[error("path component {index} is empty")]
    EmptyComponent {
        /// Component index.
        index: usize,
    },
    /// A component was `.`.
    #[error("path component {index} is `.`")]
    CurrentDirectory {
        /// Component index.
        index: usize,
    },
    /// A component was `..`.
    #[error("path component {index} is `..` (parent-directory traversal)")]
    ParentTraversal {
        /// Component index.
        index: usize,
    },
    /// A component began with `/` or `\`.
    #[error("path is absolute")]
    Absolute,
    /// A component contained an embedded path separator.
    #[error("path component {index} contains a path separator")]
    Separator {
        /// Component index.
        index: usize,
    },
    /// A component began with a Windows drive prefix such as `C:`.
    #[error("path component {index} has a Windows drive prefix")]
    DrivePrefix {
        /// Component index.
        index: usize,
    },
    /// A component contained a control character.
    #[error("path component {index} contains a control character")]
    ControlChar {
        /// Component index.
        index: usize,
    },
    /// A component contained a character Windows forbids in file names.
    #[error("path component {index} contains the forbidden character `{ch}`")]
    ForbiddenChar {
        /// Component index.
        index: usize,
        /// The offending character.
        ch: char,
    },
    /// A component ended with a dot or a space.
    #[error("path component {index} ends with a dot or a space")]
    TrailingDotOrSpace {
        /// Component index.
        index: usize,
    },
    /// A component was a reserved Windows device name.
    #[error("path component {index} is the reserved device name `{component}`")]
    ReservedName {
        /// Component index.
        index: usize,
        /// The rejected component.
        component: String,
    },
    /// A component exceeded [`MAX_COMPONENT_BYTES`].
    #[error("path component {index} is {len} bytes long (limit {MAX_COMPONENT_BYTES})")]
    ComponentTooLong {
        /// Component index.
        index: usize,
        /// Observed byte length.
        len: usize,
    },
    /// The assembled path exceeded [`MAX_PATH_BYTES`].
    #[error("path is {len} bytes long (limit {MAX_PATH_BYTES})")]
    PathTooLong {
        /// Observed byte length.
        len: usize,
    },
    /// A component was not valid UTF-8.
    #[error("path component {index} is not valid UTF-8")]
    InvalidUtf8 {
        /// Component index.
        index: usize,
    },
}

/// Validate one path component and return it as a `String`.
///
/// The returned name is a single, non-special file name. `index` is reported in
/// errors so callers can point at the offending component of a multi-component
/// path.
pub fn sanitize_component(component: &[u8], index: usize) -> Result<String, Error> {
    if component.is_empty() {
        return Err(Error::EmptyComponent { index });
    }
    if component.len() > MAX_COMPONENT_BYTES {
        return Err(Error::ComponentTooLong {
            index,
            len: component.len(),
        });
    }
    if component[0] == b'/' || component[0] == b'\\' {
        return Err(Error::Absolute);
    }
    if component == b"." {
        return Err(Error::CurrentDirectory { index });
    }
    if component == b".." {
        return Err(Error::ParentTraversal { index });
    }
    if component.contains(&b'/') || component.contains(&b'\\') {
        return Err(Error::Separator { index });
    }
    if component.len() >= 2 && component[1] == b':' && component[0].is_ascii_alphabetic() {
        return Err(Error::DrivePrefix { index });
    }
    let text = std::str::from_utf8(component).map_err(|_| Error::InvalidUtf8 { index })?;
    if let Some(_ch) = text.chars().find(|ch| ch.is_control()) {
        return Err(Error::ControlChar { index });
    }
    if let Some(ch) = text
        .chars()
        .find(|ch| matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return Err(Error::ForbiddenChar { index, ch });
    }
    if text.ends_with('.') || text.ends_with(' ') {
        return Err(Error::TrailingDotOrSpace { index });
    }
    if is_reserved_device_name(text) {
        return Err(Error::ReservedName {
            index,
            component: text.to_owned(),
        });
    }
    Ok(text.to_owned())
}

/// Validate a whole relative path and assemble it from its components.
///
/// Rejects empty paths, traversal components, absolute paths, separators inside
/// components, Windows reserved names, control and forbidden characters,
/// trailing dots/spaces, non-UTF-8 components and over-long components/paths.
///
/// ```
/// # use flux_core::path::sanitize;
/// let path = sanitize(&["movies", "film.mkv"]).unwrap();
/// assert!(path.ends_with("film.mkv"));
/// ```
pub fn sanitize<C: PathComponent>(components: &[C]) -> Result<PathBuf, Error> {
    if components.is_empty() {
        return Err(Error::EmptyPath);
    }

    let mut path = PathBuf::new();
    let mut total = 0usize;
    for (index, component) in components.iter().enumerate() {
        let name = sanitize_component(component.component_bytes(), index)?;
        total = total
            .checked_add(name.len())
            .ok_or(Error::PathTooLong { len: usize::MAX })?;
        if index > 0 {
            total = total
                .checked_add(1)
                .ok_or(Error::PathTooLong { len: usize::MAX })?;
        }
        if total > MAX_PATH_BYTES {
            return Err(Error::PathTooLong { len: total });
        }
        path.push(name);
    }
    Ok(path)
}

/// Return `true` if `name` is a reserved Windows device name.
///
/// The check is case-insensitive and looks at the name before the first
/// extension, so `CON`, `con.txt` and `CON .tar.gz` are all reserved.
pub fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let stem = stem.trim_end_matches([' ', '.']);
    let upper = stem.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    upper.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.as_bytes()[3].is_ascii_digit()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn rejects(components: &[&str], predicate: impl Fn(&Error) -> bool) {
        let error = sanitize(components).unwrap_err();
        assert!(predicate(&error), "unexpected error: {error:?}");
    }

    #[test]
    fn accepts_ordinary_and_utf8_paths() {
        let cases: &[&[&str]] = &[
            &["file.txt"],
            &["dir", "file.txt"],
            &["a", "b", "c", "d.mkv"],
            &["Café", "日本語.mkv"],
            &["emoji-🎬.mp4"],
            &[".hidden"],
            &["file.tar.gz"],
            &["CON1"],
            &["CONSOLE"],
            &["auxiliary"],
            &["com10"],
            &["lpt0x"],
        ];
        for components in cases {
            let path = sanitize(components).unwrap();
            assert_eq!(path.components().count(), components.len());
        }
        assert_eq!(
            sanitize(&["dir", "file.txt"]).unwrap(),
            PathBuf::from("dir").join("file.txt")
        );
    }

    #[test]
    fn rejects_empty_paths_and_components() {
        let none: &[&str] = &[];
        assert_eq!(sanitize(none).unwrap_err(), Error::EmptyPath);
        rejects(&[""], |error| {
            matches!(error, Error::EmptyComponent { index: 0 })
        });
        rejects(&["a", ""], |error| {
            matches!(error, Error::EmptyComponent { index: 1 })
        });
    }

    #[test]
    fn rejects_traversal_and_current_directory() {
        rejects(&["."], |error| {
            matches!(error, Error::CurrentDirectory { index: 0 })
        });
        rejects(&[".."], |error| {
            matches!(error, Error::ParentTraversal { index: 0 })
        });
        rejects(&["a", "..", "b"], |error| {
            matches!(error, Error::ParentTraversal { index: 1 })
        });
        rejects(&["..", "..", "etc", "passwd"], |error| {
            matches!(error, Error::ParentTraversal { index: 0 })
        });
    }

    #[test]
    fn rejects_absolute_paths() {
        for components in [
            &["/etc/passwd"][..],
            &["\\etc\\passwd"][..],
            &["//server/share/file"][..],
            &["\\\\server\\share\\file"][..],
        ] {
            rejects(components, |error| matches!(error, Error::Absolute));
        }
    }

    #[test]
    fn rejects_embedded_separators_and_drive_prefixes() {
        for components in [&["dir/file"][..], &["dir\\file"][..], &["a/b/c"][..]] {
            rejects(components, |error| matches!(error, Error::Separator { .. }));
        }
        rejects(&["C:"], |error| {
            matches!(error, Error::DrivePrefix { index: 0 })
        });
        rejects(&["C:autoexec.bat"], |error| {
            matches!(error, Error::DrivePrefix { index: 0 })
        });
        rejects(&["c:temp"], |error| {
            matches!(error, Error::DrivePrefix { index: 0 })
        });
        rejects(&["a:b"], |error| {
            matches!(error, Error::DrivePrefix { index: 0 })
        });
    }

    #[test]
    fn rejects_control_characters() {
        for component in ["a\u{0}b", "a\nb", "a\tb", "a\u{7f}b", "\u{1}b"] {
            rejects(&[component], |error| {
                matches!(error, Error::ControlChar { index: 0, .. })
            });
        }
    }

    #[test]
    fn rejects_forbidden_characters() {
        for (component, forbidden) in [
            ("a<b", '<'),
            ("a>b", '>'),
            ("ab:c", ':'),
            ("a\"b", '"'),
            ("a|b", '|'),
            ("a?b", '?'),
            ("a*b", '*'),
        ] {
            rejects(&[component], |error| {
                matches!(
                    error,
                    Error::ForbiddenChar { index: 0, ch } if *ch == forbidden
                )
            });
        }
    }

    #[test]
    fn rejects_trailing_dots_and_spaces() {
        for component in ["file.", "file ", "file..", "file. .", "file ."] {
            rejects(&[component], |error| {
                matches!(error, Error::TrailingDotOrSpace { index: 0 })
            });
        }
    }

    #[test]
    fn rejects_windows_device_names() {
        for component in [
            "CON",
            "con",
            "Con",
            "CON.txt",
            "con.tar.gz",
            "CON .txt",
            "NUL",
            "nul.bin",
            "PRN",
            "AUX",
            "aux.cfg",
            "CLOCK$",
            "CONIN$",
            "CONOUT$",
            "COM0",
            "COM1",
            "com9.txt",
            "LPT0",
            "LPT9",
            "lpt1.log",
        ] {
            rejects(&[component], |error| {
                matches!(error, Error::ReservedName { index: 0, .. })
            });
        }
    }

    #[test]
    fn rejects_overlong_components() {
        let long = "a".repeat(MAX_COMPONENT_BYTES + 1);
        rejects(&[&long], |error| {
            matches!(
                error,
                Error::ComponentTooLong {
                    index: 0,
                    len
                } if *len == MAX_COMPONENT_BYTES + 1
            )
        });
        let max = "a".repeat(MAX_COMPONENT_BYTES);
        assert!(sanitize(&[&max]).is_ok());
    }

    #[test]
    fn rejects_overlong_total_paths() {
        let component = "a".repeat(200);
        let components: Vec<&str> = (0..21).map(|_| component.as_str()).collect();
        rejects(&components, |error| {
            matches!(error, Error::PathTooLong { .. })
        });
    }

    #[test]
    fn rejects_invalid_utf8() {
        let components = [vec![0xff, 0xfe]];
        let error = sanitize(&components).unwrap_err();
        assert_eq!(error, Error::InvalidUtf8 { index: 0 });
    }

    #[test]
    fn accepts_byte_components() {
        let components: Vec<Vec<u8>> = vec![b"dir".to_vec(), b"file.bin".to_vec()];
        assert_eq!(
            sanitize(&components).unwrap(),
            PathBuf::from("dir").join("file.bin")
        );
    }

    #[test]
    fn reserved_name_detection_is_exact() {
        assert!(is_reserved_device_name("CON.txt"));
        assert!(is_reserved_device_name("com7.tar.gz"));
        assert!(!is_reserved_device_name("COM10"));
        assert!(!is_reserved_device_name("CONSOLE"));
        assert!(!is_reserved_device_name(".CON"));
        assert!(!is_reserved_device_name("mycon"));
    }
}
