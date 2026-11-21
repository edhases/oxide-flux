//! Pure checks for release selection: tag parsing, pre-release skipping and the
//! newer/equal/older decision. No network and no filesystem access.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use flux_app::update::newest;

#[test]
fn picks_the_highest_newer_release_in_any_order() {
    assert_eq!(
        newest("0.1.0", &["0.1.1", "v0.2.0", "0.1.9"]).as_deref(),
        Some("0.2.0")
    );
    assert_eq!(
        newest("0.1.0", &["1.0.0", "0.2.0", "0.3.1"]).as_deref(),
        Some("1.0.0")
    );
}

#[test]
fn equal_and_older_releases_are_not_offered() {
    assert_eq!(newest("1.2.3", &["1.2.3", "1.2.2", "1.0.0"]), None);
    assert_eq!(newest("0.1.0", &[]), None);
}

#[test]
fn leading_v_is_stripped_from_current_and_tags() {
    assert_eq!(newest("v0.1.0", &["v0.2.0"]).as_deref(), Some("0.2.0"));
    assert_eq!(newest("v0.1.0", &["0.2.0"]).as_deref(), Some("0.2.0"));
    assert_eq!(newest("0.1.0", &["v0.2.0"]).as_deref(), Some("0.2.0"));
    // Only one leading `v` is a tag prefix; a stray second one is malformed.
    assert_eq!(newest("0.1.0", &["vv0.2.0"]), None);
}

#[test]
fn pre_releases_never_count_as_an_update() {
    assert_eq!(
        newest("0.1.0", &["0.2.0-rc.1", "0.2.0-beta"]),
        None,
        "pre-release tags must be skipped"
    );
    assert_eq!(
        newest("0.1.0", &["0.2.0-rc.1", "0.2.0"]).as_deref(),
        Some("0.2.0"),
        "the stable release still wins over its pre-releases"
    );
    // A pre-release `current` is still offered its final release.
    assert_eq!(newest("1.0.0-rc.1", &["1.0.0"]).as_deref(), Some("1.0.0"));
    assert_eq!(newest("1.0.0-rc.1", &["1.0.0-rc.2"]), None);
}

#[test]
fn malformed_versions_are_skipped() {
    assert_eq!(
        newest("0.1.0", &["nightly", "v", "1.2", "0.2.0"]).as_deref(),
        Some("0.2.0")
    );
    assert_eq!(newest("0.1.0", &["nightly", "v", "1.2"]), None);
    assert_eq!(newest("0.1.0", &[""]), None);
}

#[test]
fn unparseable_current_offers_nothing() {
    // Refuse rather than invent an ordering.
    assert_eq!(newest("nightly", &["9.9.9"]), None);
    assert_eq!(newest("", &["1.0.0"]), None);
}

#[test]
fn build_metadata_does_not_hide_a_newer_release() {
    assert_eq!(
        newest("0.1.0", &["0.2.0+build.7"]).as_deref(),
        Some("0.2.0+build.7")
    );
}
