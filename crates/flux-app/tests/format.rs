//! Pure formatting and playback-detection helpers used by the desktop UI.

use flux_app::{format_bytes, format_duration, is_media};

#[test]
fn bytes_are_human_readable() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(999), "999 B");
    assert_eq!(format_bytes(1024), "1.0 KiB");
    assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
    assert_eq!(format_bytes(1536), "1.5 KiB");
    assert_eq!(format_bytes(5 * 1024_u64.pow(3)), "5.0 GiB");
}

#[test]
fn durations_look_like_etas() {
    assert_eq!(format_duration(5), "5s");
    assert_eq!(format_duration(65), "1m 5s");
    assert_eq!(format_duration(3 * 3600 + 4 * 60), "3h 4m");
}

#[test]
fn media_detection_covers_the_common_containers() {
    for path in [
        "movie.mkv",
        "Movie.MP4",
        "clip.webm",
        "song.flac",
        "track.mp3",
        "stream.ts",
    ] {
        assert!(is_media(path), "{path} should be playable");
    }
    for path in ["readme.txt", "archive.rar", "setup.exe", "no-extension"] {
        assert!(!is_media(path), "{path} should not be playable");
    }
}
