# SignPath Foundation application — prepared answers

[SignPath Foundation](https://signpath.org/) signs open-source Windows builds
for free, using a certificate SignPath holds in its HSM. Nothing is installed
here and no certificate is bought; the maintainer submits this application,
SignPath reviews it by hand, and after approval the signing job in
[`docs/release.md`](release.md#signpath-wiring-recommended-not-yet-enabled)
is enabled with the organization's API token.

The answers below match the repository as it is today. Fill in the contact
details and submit — do not change the technical facts without changing the
build.

## Project

| Field | Answer |
| --- | --- |
| Project name | Oxide Flux |
| Repository | <https://github.com/edhases/oxide-flux> |
| License | GPL-3.0-or-later (`LICENSE` is the verbatim GPLv3 text) |
| Language / build | Rust 2024, Cargo workspace |
| Description | Streaming-first BitTorrent client: a headless engine (`fluxd`) plus a native desktop client, with hash-verified streaming playback of media files while the torrent is still downloading. |
| Website / docs | <https://github.com/edhases/oxide-flux#readme> |
| Maintainer | edhases (repository owner) |
| Users | Distributed as public GitHub releases; no account, telemetry or paid tier. |

## Why a signature

Windows SmartScreen warns about unsigned installers, which is the main
obstacle to distributing the per-user NSIS setup. The project has no budget
for a commercial certificate, and post-2023 CA/B rules make a new OV/EV key
non-exportable, so a cloud signing service is the only practical route.

## Build and release process

| Question | Answer |
| --- | --- |
| CI system | GitHub Actions |
| CI config | `.github/workflows/ci.yml` (fmt → clippy `-D warnings` → tests, plus `cargo-deny`) and `.github/workflows/release.yml` (on `v*` tags) |
| Build command | `cargo build --release --locked --bin oxide-flux --bin fluxd` |
| Binaries signed | `oxide-flux.exe` (GUI), `fluxd.exe` (headless daemon), `oxide-flux-<tag>-windows-x86_64-setup.exe` (NSIS installer) |
| Installer | `installer/oxide-flux.nsi`, compiled with NSIS in the release job; per-user, no elevation |
| Who can trigger a release | The repository owner only; `main` is protected and requires the `check (ubuntu-latest)`, `check (windows-latest)` and `deny` checks |
| Release artifacts | Windows zip, Windows setup `.exe`, Linux tarball, SPDX SBOM, `SHA256SUMS`, keyless cosign bundle |
| Existing integrity | `SHA256SUMS` plus a cosign `--bundle` signature over it, both attached to every release |

Only those four binaries are submitted for signing. No third-party
executables are bundled: the optional playback engine (mpv) is downloaded by
the user from its upstream project and is never redistributed here.

## Signing details to request

| Field | Value |
| --- | --- |
| Signing policy | `release-signing` (release artifacts only) |
| Artifact configuration | `oxide-flux-windows` (the `dist/` artifact of the `build` job) |
| Signing type | Authenticode, SHA-256, RFC 3161 timestamp |
| Product name | Oxide Flux |
| Product version | release tag without the leading `v` |

## After approval

1. Add repository secrets `SIGNPATH_API_TOKEN` and
   `SIGNPATH_ORGANIZATION_ID`.
2. Enable the `sign` job described in
   [`docs/release.md`](release.md#signpath-wiring-recommended-not-yet-enabled)
   and make `publish` depend on it.
3. Tag the next release and confirm that `signtool verify /pa` passes on the
   installer and both executables, then update the "not Authenticode-signed"
   note in `README.md`.
