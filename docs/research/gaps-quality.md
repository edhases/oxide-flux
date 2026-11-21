# Oxide Flux — Reliability, Security and Distribution Audit

Date: 2026-11-21. Scope: `AGENTS.md`, all six crates, `tests/`, `.github/workflows/ci.yml`,
manifests/lockfile, `docs/design/`, `crates/flux-app/assets/`. No source changed.

Snapshot: `cargo test --workspace` passes **421 unit/integration tests + 7 doctests**.
`cargo audit` (advisory DB 2026-11-21) reports **no vulnerabilities** and 5 unmaintained
transitive crates (`paste`, `rustybuzz`, `ttf-parser`, `zbus_names`, `zvariant`).
Local release binaries: `oxide-flux.exe` 43.7 MB, `fluxd.exe` 14.2 MB.

Effort: S/M/L. Priority: **P0/P1 = must fix before 1.0; P2 = post-1.0** unless a row says otherwise.

## 1. Testing

Coverage by area (green today): `flux-core` 107 unit (bencode 20, metainfo 20, magnet 36, path 14,
picker 17); `flux-net` 150 unit; `flux-disk` 3 unit + 12 integration; `flux-daemon` 47 unit +
44 integration (session 16, e2e 7, transfer 5, magnet 9, api 7); `flux-api` 21 unit + 25
integration; `flux-app` 12. Strong adversarial path/bencode tests exist
(`flux-core/src/path.rs:303-448`, `metainfo.rs:1507`), but no fuzz target and no external client.

| ID | Item | Current state | Evidence | Risk / impact | Mitigation | Effort | Prio |
|---|---|---|---|---|---|---|---|
| T1 | Fuzzing | Absent for bencode, metainfo, tracker HTTP/UDP, DHT KRPC, peer wire, magnet, ut_metadata. Only hand-written corpora and one mutation loop. | No `fuzz/`; no libfuzzer/proptest dev-deps in any `Cargo.toml`; `flux-core/src/metainfo.rs:1507` | Parser panic, memory amplification or infinite work reachable from network/tracker/peer; regressions unnoticed. | cargo-fuzz targets per parser, seed corpora, `-max_len`, OSS-Fuzz CI smoke; proptest for path/magnet. | M | P1 |
| T2 | Real-client interop | Absent. Tests connect two in-process `SessionHandle`s only. | `flux-daemon/tests/transfer.rs:305-530`; `e2e.rs:174-252` | Silent incompatibility with qBittorrent/libtorrent/Transmission (handshake reserved bits, extended handshake, choking, request pacing). | Linux CI job with transmission-daemon/qbittorrent-nox/libtorrent harness; seed/leech matrix, IPv6 and partial-seed cases. | M | P1 |
| T3 | Soak / chaos | Absent. Transfers are 64 KB e2e and ~1 MiB swarm payloads. | `e2e.rs:177-182`; `transfer.rs:4` | FD/thread/memory growth, stalls after peer churn, state drift after many restarts. | 24-72 h soak: 100+ torrents, random disconnect/choke/restart, assert RSS/FD/threads and final hashes. | L | P1 |
| T4 | Disk benchmark at 500+ Mbit/s | Absent. Writer does one positional write per complete piece; 32 MiB per-swarm cache; no measured throughput. | `flux-disk/src/writer.rs:64-73,404-433`; no criterion/iai dep | "Streaming-first" and 500 Mbit/s claims unproven; 4Kn/HDD behaviour unknown. | Criterion bench: 1-8 GB writes, 500/1000 Mbit/s, warm/cold cache, iostat + CPU; record baseline in repo. | M | P1 |
| T5 | Deterministic time | Partial. Trackers/rate limiter inject `now`; swarm/writer call `Instant::now`; e2e waits real seconds. | `e2e.rs:148`; `swarm.rs:868,1677`; `writer.rs:391` | Slow-CI flakes and timing races hidden. | Inject clock into swarm/writer; `tokio::time::pause`; loom for writer/actor handoffs. | M | P2 |
| T6 | CI matrix | ubuntu + windows only; stable toolchain; no `--locked`, audit/deny, MSRV, release build or coverage; actions pinned by tag. | `.github/workflows/ci.yml:15-32` | macOS and future-toolchain breakage; supply-chain drift; MSRV 1.99 unverified. | Add macOS, `--locked`, cargo-audit/deny, MSRV 1.99, release artifact, pinned action SHAs. | M | P1 |
| T7 | Windows-specific tests | None for registry, long paths, ACLs, multi-user or two-process queue. | `platform.rs:221-232` tests only string building | Core Windows integration is untested code. | Disposable HKCU test key, >260-char path fixture, two-process queue/ACL test. | M | P1 |
| T8 | Coverage measurement | Absent. | No llvm-cov/nextest config | Blind spots are guesses. | Coverage job with threshold, nextest for speed. | S | P2 |

## 2. Security

| ID | Item | Current state | Evidence | Risk / impact | Mitigation | Effort | Prio |
|---|---|---|---|---|---|---|---|
| S1 | Loopback API CSRF / DNS rebinding | **Real bug, verified.** Default `api_token=None`; `/rpc` and `/ws` ignore `Origin` and `Host`; `text/plain` JSON is accepted. A request with `Origin: http://evil.example`, no token, returned HTTP 200 and `session.info`. | `flux-api/src/http.rs:183-192,229-232,260-272`; `ws.rs:88-95`; `flux-daemon/src/config.rs:120,145` | A malicious page that can reach loopback (DNS rebinding, or browsers without Private Network Access preflight) can add/remove torrents (including `deleteFiles`), pause/resume, and read names via WebSocket; any local process can do the same. SSRF via attacker tracker URLs. | Generate a token per daemon install by default; require auth on loopback too; reject non-loopback `Host` and cross-site `Origin`; require `application/json` + CSRF header; better: named pipe/Unix socket. | S/M | P0 |
| S2 | Symlink/junction escape before validation | Sanitization is lexical; files are opened and `set_len`-grown, **then** canonicalized under root. Windows reparse points and Unix symlinks are followed during preallocation. | `flux-daemon/src/verify.rs:308-312,350-376`; `flux-disk/src/backend.rs:147-162` | A pre-existing link in the download root (common: media junction) lets a torrent resize/shrink a file outside the root before it is rejected; data loss. | Check `symlink_metadata`/canonical parent and leaf **before** open; open no-follow (`FILE_FLAG_OPEN_REPARSE_POINT`, `O_NOFOLLOW`); re-validate by file id after open. | M | P0 |
| S3 | `deleteFiles` follows directory links | Removal joins sanitized paths and calls `remove_file`; no canonical/link check at removal time. | `verify.rs:386-409` | If a directory is replaced by a junction/symlink after add, "remove and delete files" deletes outside the root. | Canonicalize each target and parent at delete; refuse links; delete via open handle / no-follow. | M | P1 |
| S4 | Windows reserved names | `CON`, `PRN`, `AUX`, `NUL`, `COM0-9`, `LPT0-9`, `CLOCK$`, `CONIN$`, `CONOUT$` are covered; superscript digit variants such as `COM¹` are not. | `flux-core/src/path.rs:240-253` | Possible DOS-device aliasing instead of a normal file, causing hangs or writes to a device on affected Windows versions. | Use `RtlIsDosDeviceName_U`/`GetFullPathName` semantics or explicitly reject `¹²³` variants and trailing-dot aliases. | S | P2 |
| S5 | Single-instance queue injection | Second process writes JSON into `%APPDATA%\OxideFlux\instance\requests`; primary deserializes and opens any `Torrent(PathBuf)` or `Magnet`, with no size/owner/type check. | `flux-app/src/single_instance.rs:75-96,50-67`; `launch.rs:258-292`; `app.rs:449-469` | Another **same-user** process can make the app open arbitrary files/magnets; cross-user is blocked only by default `%APPDATA%` ACLs, which code never verifies. Low privilege boundary but weak. | Explicit DACL on instance dir, cap file size, require `.torrent` extension and `is_file`, reject symlinks, verify owner SID, stale-file cleanup. | S | P2 |
| S6 | Token storage and WebSocket token | Token is plaintext TOML; no generation; no file ACL; web UI stores it in `localStorage`; WS accepts `?token=` so it lands in URL/history/logs. | `config.rs:119-120`; `static/app.js:4-6,56-58,78-79`; `ws.rs:83-95` | Token theft by same-user process/malware or shared profile; leak through logs/proxies. | DPAPI/Keychain/secret-service, 0600 config, auto-generate, header/subprotocol auth for WS, rotate command. | M | P1 |
| S7 | CLI argument handling | No `--` terminator; options accepted anywhere; a target beginning `-` is an unknown-flag error. No shell invocation, so no command injection. | `flux-app/src/launch.rs:118-143`; `flux-daemon/src/cli.rs:287-364` | Legitimate hyphen-leading paths/URIs cannot be opened; confusing UX only. | Support `--`, document option order, validate target count. | S | P2 |
| S8 | Magnet validation | Core parser is strict and bounded: 8 KiB URI, percent-decoding, hash family/conflict checks, 256 trackers/web seeds, 64 unsupported URNs. RPC re-checks via engine; only a cheap prefix check precedes it. | `flux-core/src/magnet.rs:41-55,170-218,390-414`; `flux-api/src/methods.rs:151-159` | Residual: lowercase-only `magnet:` rejects mixed-case OS launches; `x.pe`/`xs` ignored (documented). No injection. | Accept case-insensitive scheme per RFC 3986, keep strict decode; add hostile-magnet fuzz corpus. | S | P2 |
| S9 | API auth timing / brute force | Constant-time comparison via `subtle`; length observable (documented); any non-empty token accepted, no rate limit or entropy check. | `flux-api/src/http.rs:274-278`; `config.rs:285-287` | Weak operator token can be brute-forced if exposed beyond loopback; substring-length oracle only. | Enforce minimum length/entropy, rate-limit 401s, lockout/backoff, audit log. | S | P2 |
| S10 | DHT privacy for private torrents | Both runtime checks exist: `poll_dht` skips private and `on_dht_peers` refuses them. However magnet metadata bootstrap uses DHT **before** the private flag is known when the link has no trackers. | `session.rs:1842-1843,1864-1870`; `session.rs:447-455` | A private magnet without trackers leaks its info hash to the DHT during metadata fetch. | Require explicit opt-in for DHT metadata bootstrap, or refuse trackerless magnets until metadata is supplied; document. | S | P2 |
| S11 | Dependency / license compliance | Assets ship licenses (Lucide ISC, Inter OFL). Workspace crates have **no `license` or `repository`** (only `flux-api` has a description); no root `LICENSE`, no third-party notices. `cargo audit`: 0 vulnerabilities, 5 unmaintained warnings. mpv is external (spawned), so no GPL binary is distributed today. | `crates/flux-app/assets/icons/LICENSE.txt:1`; `assets/fonts/OFL.txt:1`; `Cargo.toml:12-15`; root has no `LICENSE`; `player.rs:65-80` | Cannot legally call it distributable; bundling mpv later triggers GPL source/notice obligations. | Choose project license, add fields per crate, `THIRD_PARTY.md`, CI `cargo deny` + `cargo audit`; decide mpv bundling/GPL with legal review. | M | P1 |
| S12 | Registry scope and cleanup | Writes HKCU only (good). But every GUI start re-registers and overwrites `Software\Classes\magnet` unconditionally, possibly stealing another client's handler; `unregister` uses a `contains("oxide-flux")` substring test. | `platform.rs:41-146,184-189`; `launch.rs:235-250` | User-hostile association changes and accidental deletion of another handler whose path contains the string. | Register only when absent or after explicit consent; compare exact exe path; installer-driven uninstall. | S | P2 |
| S13 | Path policy vs content compatibility | Windows rules applied on all OS: `:` and non-UTF-8 components are hard errors; no rename/quarantine path. | `path.rs:153-198`; `verify.rs:191-218` | Legitimate Linux/legacy torrents fail outright; users see "unsafe path" with no recourse. | Optional per-torrent rename policy, warning + skip file, or documented refusal with UI explanation. | M | P2 |
| S14 | Windows long paths | Sanitizer permits 4096-byte relative paths. `std::fs` handled a 354-byte path in a local check, but there is no `longPathAware` manifest and no test; external mpv/Explorer/player paths may still fail. | `path.rs:17-18`; `backend.rs:147-153`; no `build.rs`/manifest | Deep torrents may fail only when handed to external tools, after files exist; partial state. | Add `longPathAware` manifest, prefix external-tool paths, add a >260-char e2e test, or enforce 260 with a clear error. | S | P2 |

## 3. Operations

| ID | Item | Current state | Evidence | Risk / impact | Mitigation | Effort | Prio |
|---|---|---|---|---|---|---|---|
| O1 | Logging / tracing | Absent. No `log`/`tracing` dependency; only `println!`/`eprintln!` in CLI and UI. Tracker/DHT/peer errors are dropped or shown only as one banner. | All crate `Cargo.toml`s; `cli.rs:501-523`; `app.rs:254` | Field failures are undiagnosable; no audit trail; support burden. | `tracing` + rolling file logs, redact tokens/magnet names, `--log-level`, per-torrent error events. | M | P1 |
| O2 | Panic handling | Default unwind, no hook. A panic in the swarm forwarder closes the channel silently; the torrent stays "downloading" forever. | `session.rs:1721-1731`; no `set_hook`/restart logic | Silent hang with no error; actor panic stops the whole session/API. | Panic hook to crash log; monitor `JoinHandle`; surface `SwarmEvent::Failed` on channel close; decide restart policy. | M | P1 |
| O3 | Crash reporting | Absent, no opt-in. | absent | Recurring field crashes invisible. | Opt-in local crash bundle (log tail, version, state schema) or Sentry with consent. | L | P2 |
| O4 | Config/state schema versioning | State has version 1, strict refusal of unknown versions; config has no version and `deny_unknown_fields`; settings has no version. | `state.rs:32,230-258`; `config.rs:92-94`; `launch.rs:50-55` | Every config addition breaks older builds; no migration path; downgrade needs manual state deletion. | Config `version` + migrations; state v1→v2 migration framework; golden-file migration tests. | M | P1 |
| O5 | Corrupt-state recovery | Any corrupt JSON/hex/version is a hard startup error; no `.bak`, quarantine or UI action. | `state.rs:14-17,216-259`; `session.rs:386-399` | One bad byte prevents the daemon starting; user loses queue to recover. | Keep last-good backup, rename corrupt file to `.corrupt-<ts>`, start empty with warning, export/import. | S | P1 |
| O6 | Atomic writes | State is tmp + `sync_all` + rename and cleans up on failure (good). Settings write has no fsync; request files fsync but parent directory is not synced. | `state.rs:289-309`; `launch.rs:81-89`; `single_instance.rs:50-67` | Small durability window after power loss for settings/queue. | Reuse the state helper; fsync parent on Unix; test simulated rename failure. | S | P2 |
| O7 | Disk-full behaviour | Any per-piece write error (including ENOSPC) is requeued as `PieceCorrupt` and retried indefinitely; no error classification, backoff or stop. | `swarm.rs:1373-1393`; `writer.rs:469-486` | Endless re-download/spin, misleading "corrupt" state, no actionable user error, possible network waste. | Classify `StorageFull`/`PermissionDenied`, pause torrent with fatal error, exponential backoff, reserve free space. | M | P1 |
| O8 | Graceful shutdown | CLI Ctrl-C persists and stops swarms. The GUI never sends `Command::Shutdown`; closing the window drops `_runtime`, so the supervisor's `session.shutdown()` may be cancelled before state flush/fsync/tracker `stopped`. mpv is killed on `Drop` but orphaned on crash. | `app.rs:221,246-296`; `supervisor.rs:55-112`; `session.rs:1309-1323` | Lost last state change, unflushed pieces, no `stopped` announce; orphan player process. | Window-close task sends Shutdown and awaits it, `exit_on_close_request(false)`; Windows Job Object for mpv; shutdown deadline. | M | P1 |
| O9 | Portable / multi-user | No portable mode; paths hardcoded to `%APPDATA%`/`USERPROFILE` (only downloads and mpv overridable by env). No installer or per-machine mode. HKCU is correct for per-user. | `launch.rs:145-161`; `app.rs:1358-1373`; `platform.rs:33-47` | Cannot run from USB/read-only media; enterprise deployment and multi-user machines unsupported. | `--portable` / `OXIDE_FLUX_HOME`, config-dir override, document per-user model. | S | P2 |
| O10 | Restore scalability | Every persisted torrent is added concurrently at startup (prepare + preallocate + verify); no stagger or cap. | `session.rs:1167-1175`; `1364-1379` | 1000 torrents at boot → FD/thread/memory spike, slow or failed start. | Bounded restore queue, one-at-a-time prepare, defer verification, progress in UI. | M | P2 |
| O11 | Docs drift | `flux-daemon` crate docs and config comments still say no peer networking/API, while the code transfers and serves. | `flux-daemon/src/lib.rs:3-7`; `config.rs:30,42` | Misleads operators and future maintainers. | Regenerate crate docs from `AGENTS.md` truth; add doc test/check. | S | P2 |

## 4. Distribution

| ID | Item | Current state | Evidence | Risk / impact | Mitigation | Effort | Prio |
|---|---|---|---|---|---|---|---|
| D1 | Windows installer | Absent (no MSI/NSIS/WiX/MSIX). Only a bare `.exe`; no install layout, Start Menu, uninstall entry or upgrade logic. | `.github/workflows/` contains only `ci.yml`; no `build.rs`/installer files | Cannot ship a 1.0 Windows product. | Per-user WiX/MSIX installer, Start Menu shortcut, uninstall entry; optional per-machine MSI. | M | P0 |
| D2 | GUI subsystem, icon, manifest | No `#![windows_subsystem = "windows"]`, so association launches show a console window. No embedded `.ico`, version resource, DPI or `longPathAware` manifest. | `flux-app/src/main.rs:1`; no `build.rs`/`.rc` | Unprofessional UX; console flashes; generic icon in Explorer/Alt-Tab; long-path failures (S14). | Add subsystem attribute, `winres`/`embed-resource` with icon/version/manifest, declare DPI awareness. | S | P0 |
| D3 | Code signing / SmartScreen | Absent. | no signing config/workflow | SmartScreen warnings, AV blocks, no enterprise deployment. | OV/EV code-signing cert, sign exe + installer, timestamp, reputation build-up. | L | P1 |
| D4 | Auto-update | Absent; no signed manifest or verification design. | absent | Unpatched clients after security fixes (especially S1). | Ed25519/minisign-signed manifest over HTTPS, staged rollout/rollback, opt-in; or explicit "no updater" decision. | M/L | P1 |
| D5 | Uninstall / association cleanup | `--unregister` exists but is never invoked by an uninstaller; state in Downloads and `%APPDATA%` is left behind. | `platform.rs:155-199`; `launch.rs:213-224` | Stale magnet/.torrent handlers and multi-GB residue after uninstall. | Installer runs `--unregister`; offer keep/remove data; delete AppData settings. | S | P1 |
| D6 | Portable build | Absent. | `launch.rs:147-161` | No USB/read-only deployment. | Portable ZIP with `--portable` layout, relative state dirs. | S | P2 |
| D7 | Linux packaging | Absent: no `.desktop`, MIME XML, hicolor icons, AppImage/deb/flatpak; associations return an error on non-Windows. CI installs GUI deps only. | `platform.rs:201-219`; `ci.yml:23-28` | Linux desktop users cannot install or open magnets/.torrent by default. | `.desktop` + `MimeType`, shared-mime-info XML, icons, AppImage/deb; `xdg-mime` registration. | M/L | P2* |
| D8 | macOS package | Absent: no `.app`, Info.plist, UTI declarations, notarization, associations. | absent | Not shippable on macOS. | App bundle + UTI/CFBundleDocumentTypes, codesign/notarize, dmg. | L | P2* |
| D9 | CI release pipeline | Absent: only fmt/clippy/test. No tagged builds, artifacts, checksums, SBOM or provenance. | `ci.yml:1-32` | Releases are manual and unverifiable. | Tag-triggered matrix build, checksums, SBOM (cargo-cyclonedx), GitHub attestations, draft release. | M | P1 |
| D10 | Versioning / docs | Workspace 0.1.0; no README, CHANGELOG, SECURITY.md or download page. API version is 1. | `Cargo.toml:12-15`; root listing | Users cannot judge compatibility or report vulnerabilities. | SemVer policy, CHANGELOG, SECURITY.md, README with install/verify steps. | S | P2 |

\* P2 only if 1.0 is declared Windows-only; otherwise the relevant platform package is P1.

## 5. Performance

| ID | Item | Current state | Evidence | Risk / impact | Mitigation | Effort | Prio |
|---|---|---|---|---|---|---|---|
| P1 | Release profile | `lto = "thin"`, `codegen-units = 1`; no strip, no `panic = "abort"`, no PGO/BOLT. | `Cargo.toml:17-19`; binary sizes above | 43.7 MB app with symbols; slower startup/IO. | `strip = "symbols"`, consider `panic = "abort"` after O2, optional PGO release job. | S | P2 |
| P2 | Runtime sizing | GUI hardcodes 2 tokio workers; daemon uses 2-8 based on cores; verifier 1-8; no benchmark justifies values. | `app.rs:247-250`; `config.rs:146-156` | Under-parallelised on many cores or oversubscribed on small machines; disk/CPU contention. | Benchmark worker/verifier/upload-slot combinations; default `min(4, cores)`; expose settings UI. | S | P2 |
| P3 | Memory ceiling at scale | Untested. 32 MiB writer cache per active swarm, bounded peer queues, but unbounded app mpsc channels and no max torrent count/RSS measurement. | `writer.rs:64-73`; `app.rs:262-264`; `swarm.rs:127-137` | OOM with many torrents/peers; OS kill loses state. | Soak with 100/1000 torrents, assert RSS/FD/handles, bound channels, per-torrent cache budget. | M | P1 |
| P4 | Table rendering | Eager: `scrollable(self.table(&visible))` builds every row each view; 100 ms pump, re-filter/sort each frame. | `app.rs:298-305,712,844-915` | UI stutter at hundreds of torrents. | Virtualised/`lazy` list, stable row keys, update only changed summaries. | M | P2 |
| P5 | Startup time | Unmeasured; every launch reads settings, writes registry associations and starts verification for all restored torrents. | `launch.rs:226-251`; `session.rs:1167-1175` | Slow cold start, AV/EDR noise from registry writes. | Measure cold/warm start; register only on change; stagger restore (O10). | S | P2 |
| P6 | Allocator / I/O backend | System allocator; `StdBackend` positional I/O per piece; no coalescing across complete pieces; no `io_uring`/IOCP. | `flux-disk/src/backend.rs:146-193`; AGENTS.md next work | Throughput ceiling and allocator contention at high rates. | Benchmark first (T4); then consider mimalloc and native backend behind `BlockBackend`. | L | P2 |

## Top-10 pre-1.0 checklist

1. **S1/S6** — Make the control API authenticated by default, reject cross-site `Origin`/foreign `Host`, require JSON content type, store the token outside plaintext/localStorage, and test the CSRF/rebinding cases.
2. **S2/S3** — Validate links and canonical paths **before** opening/preallocating, and re-check at delete; add junction/symlink regression tests.
3. **D1/D2/D5/T7** — Ship a per-user Windows installer with a real GUI-subsystem binary, icon/version/manifest, Start Menu entry and uninstaller that runs `--unregister`; add registry, long-path and multi-user tests.
4. **D3/D9/D4** — Code-sign exe + installer, add a tag-triggered release pipeline with checksums/SBOM/provenance, and make the auto-update decision.
5. **O1/O2** — Add structured logging with redaction and a panic hook; turn silent swarm/actor death into a visible error.
6. **O7** — Classify disk-full/permission failures, pause the torrent with an actionable error, add backoff; never loop re-downloading a failing piece.
7. **O8** — Make window close await session shutdown (flush, fsync, tracker `stopped`) and guarantee mpv cleanup.
8. **O4/O5** — Add config/state schema versions + migrations, a last-good state backup and automatic quarantine of corrupt state.
9. **T1/T2/T3/T4** — Add fuzz targets, a real-client interop job, a soak run and a 500+ Mbit/s disk benchmark to CI.
10. **T6/S11/P3/P4** — Add `--locked` + cargo-audit/deny + macOS CI, root/per-crate licensing, a 1000-torrent RSS ceiling, and a virtualised transfer table.

## Post-1.0 / deliberately deferred

- Protocol features: MSE/PE, uTP/LEDBAT, PEX, LSD, web seeds, HTTP/UDP scrape and v2 Merkle
  verification. MSE and uTP are the most likely to block private-tracker and real-swarm interop.
- macOS bundle and Linux packages **if** 1.0 is declared Windows-only.
- Auto-update, crash reporting, portable mode, PGO/alternative allocator, coverage thresholds.

## Assumptions not verified

- No real BitTorrent client, tracker, DHT bootstrap or public swarm was contacted; interop is
  inferred from codec tests only.
- Only Windows was inspected/run; Linux/macOS behaviour, packaging and ACLs were not executed.
- The symlink/junction exploit in S2/S3 was **not** run (would require creating links); the
  conclusion comes from the call order in `verify.rs`.
- `%APPDATA%\OxideFlux` ACLs were assumed to be the Windows default per-user DACL; no multi-user
  or roaming-profile test was performed.
- `cargo audit` reflects the advisory DB on 2026-11-21; transitive licenses were read from
  crates.io metadata, not reviewed file by file.
- No installer/signing/auto-update artefacts exist, so those rows are absence claims.
- Throughput, startup time and RSS were not measured; binary sizes are from this machine's
  local release build.
- The repo is not a git repository, so CI history, branch protection and release tags could not
  be checked.
