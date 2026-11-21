# AGENTS.md — Oxide Flux

Streaming-first BitTorrent client in Rust: headless engine (`fluxd`) + separate UI over
JSON-RPC / WebSocket. It **really transfers**: TCP peer wire, HTTP/UDP trackers, mainline
DHT, `ut_metadata` magnets, hash-verified downloads and seeding, session persistence.
Still absent: MSE/PE, uTP, PEX, LSD, web seeds, and v2 Merkle verification — a v2-only
torrent is stored but never downloaded. Never invent bytes, speeds or peer counts.

Repository: `github.com/edhases/oxide-flux`, branch `main`. Commit messages are
plain imperative summaries; no PR template yet.

## Commands

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings   # warnings are errors
cargo test --workspace
cargo test -p flux-daemon --test e2e                    # loopback transfers
cargo test -p flux-daemon --test magnet                 # ut_metadata fetch
cargo test -p flux-core piece_boundary                  # single test
cargo nextest run --workspace                           # installed, faster
```

Real entry points:

```sh
cargo run -p flux-app --bin oxide-flux                 # native desktop client
cargo run -p flux-daemon --bin fluxd -- verify file.torrent --dir D:/x [--json]
cargo run -p flux-daemon --bin fluxd -- --config fluxd.toml   # API + WebUI until Ctrl-C
# then open http://127.0.0.1:6800/  (paste api_token when the config sets one)
```

The desktop client registers `.torrent` and `magnet:` for the current user on
startup (per-user `HKCU`, no admin). `--register` / `--unregister` / `--no-register`
control it explicitly; a second launch hands its target to the running window.

CI (`.github/workflows/ci.yml`) runs fmt → clippy → test on ubuntu + windows.

## Crate map

- `flux-core` — strict bencode, metainfo v1/v2/hybrid, magnet parsing, path sanitation,
  pure `Picker` (sequential/rarest/endgame). No I/O.
- `flux-disk` — `BlockBackend`, torrent-space `FileLayout`, coalescing `DiskWriter`,
  threaded SHA-1 `Verifier`.
- `flux-net` — bitfield, handshake, message codec, payload-only rate limiter; `peer`
  (TCP session actor), `tracker` (HTTP/ BEP 15), `dht` (BEP 5), `metadata` (BEP 9).
- `flux-api` — JSON-RPC over HTTP + WebSocket, `Engine` trait, `MockEngine`, bearer auth,
  and an embedded static WebUI (`static/`, served at `/` with a CSP; no build step).
- `flux-daemon` — config, session actor, swarm (peers/requests/choking/uploads), tracker
  announce service, magnet fetch, loopback streaming server, state persistence, `Engine`
  adapter, `fluxd` CLI.
- `flux-app` — native iced 0.14 desktop client: in-process session, design tokens
  (`theme.rs`/`style.rs`), embedded Inter + Lucide assets, launch/association/single-instance
  handling, embedded mpv playback with a loopback stream URL.

Dependency direction: `daemon → api → (nothing)`, `daemon → core + disk + net`,
`net → core`. Don't make `core`/`disk`/`net` depend on `api` or `daemon`.

## Gotchas that cost time

- **Inbound handshake is read once.** The session listener reads 68 bytes to route by info
  hash, then calls `peer::accept_prefixed`; plain `accept` would wait for a second
  handshake and silently drop every inbound peer.
- **The transport drains all buffered frames.** Do not re-introduce per-frame pacing: a
  burst of requests/haves arriving in one `read` must be delivered immediately.
- **Two `FileLayout`s.** Core's v2 layout has alignment gaps; the disk one needs
  `allow_gaps: true`. The only conversion is `flux-daemon/src/verify.rs::plan`, proven by
  `check_layouts_agree`. Never re-derive offsets.
- **`listen_port = 0` means ephemeral**, not disabled: the session binds a real port and
  announces it, which is what makes tracker discovery work.
- **Private torrents never touch the DHT** (`poll_dht` skips them; `on_dht_peers` refuses
  again). Keep both checks.
- **`complete` means every *desired* piece**, not every piece: selective downloads complete
  with skipped files missing. A piece straddling a skipped file is still fetched whole.
- **Persistence replays `add`**: `SessionHandle::start` loads `.fluxd-state.json` and pushes
  each torrent through the normal add path, then applies priorities and `paused`. Corrupt or
  version-2 state is a hard error, never silently dropped.
- **Background saves are serialized by their `JoinHandle`, not by a completion message.**
  `flush_persist` sees a finished write through `save_task.is_finished()`, and
  `persist_now` awaits the in-flight handle before its final synchronous write. Awaiting is
  required because `save_async` runs on `spawn_blocking` and aborting would not stop the
  write; without this, a stale snapshot could land after shutdown and resurrect old state.
- **Tests must stay offline.** Tracker/DHT tests use loopback sockets only; every daemon
  test config sets `enable_dht: false` (otherwise bootstrap DNS resolution runs).
- **The UI is embedded, not built.** `flux-api/static/{index.html,app.js,style.css}` are
  `include_str!`-ed; edit them directly, keep them vanilla (no bundler), and remember the
  static routes are public while every data call still needs the token. `tests/ui.rs` pins
  both facts.
- **Associations are path-bound.** `HKCU\Software\Classes` points at the exact
  `oxide-flux.exe` that registered them; after moving or switching debug↔release, start that
  build once (or run `--register`). Windows `UserChoice` can still override `.torrent`, so a
  conflict is surfaced instead of claimed as success.
- **Single instance is a lock plus a request queue**, not a socket:
  `%APPDATA%\OxideFlux\instance\instance.lock` + `requests/*.json`. A second launch writes a
  request and exits; the primary drains it every 250 ms. Keep writes atomic
  (`.tmp` then rename) or the window will read half a URI.
- **Drag & drop accepts `.torrent` only** (`window::Event::FileDropped`); magnets arrive via
  paste, CLI, or the `magnet:` association.
- **Desktop playback is embedded mpv.** `player::play_embedded(parent_hwnd, bounds, path, title)`
  creates a child Win32 window and runs mpv with `--wid`; resize via `set_bounds`. If mpv is
  missing or the HWND is unavailable, it falls back to the external `play`. Do not describe
  playback as external-only.
- **Streaming serves verified bytes only.** `flux-daemon/src/stream.rs` is a loopback HTTP
  range server; `stream_source.rs` polls `piece_state` until every covering piece is verified,
  then reads through `FileSet`. Never return unverified bytes, and keep the server bound to
  loopback (it has no auth). `Engine::stream_url` caches one server per torrent.
- **Play flow:** the UI asks the supervisor for a stream URL; the supervisor sets sequential
  mode + playhead to the file's first piece, then the app resolves the window HWND, scale and
  size and calls `play_embedded` with a physical-pixel rectangle.
- **Playback engine is user-selectable.** Settings chooses Bundled / System / Custom / Disabled.
  "Install playback engine" downloads a pinned, SHA-256-verified GPL mpv build into
  `%APPDATA%\OxideFlux\playback\mpv` (never system-wide); `Remove playback engine` deletes it.
  Keep the pinned version and digest in `flux-app/src/playback.rs` in sync with THIRD_PARTY.
- **Auto-update is signed by checksums, not Authenticode.** `flux-app/src/update.rs` checks
  GitHub releases, pins the audited tag, verifies `SHA256SUMS` + the GitHub asset digest, and
  only reports success when `self_update` actually replaced the binary. A failed check is
  silent. Do not loosen the version pinning.
- **Release assets** come from `.github/workflows/release.yml`: Windows NSIS setup, Windows zip,
  Linux tarball, SPDX SBOM, `SHA256SUMS`, and a keyless cosign bundle. Signing options and the
  post-2023 no-PFX rule are documented in `docs/release.md`.
- **A block must be requested from the peer that sends it.** `Picker` rejects unsolicited
  blocks (`BlockNotRequested`); late duplicates are ignored without punishing the peer, and a
  corrupt piece records its contributors. A sole contributor is banned, as is a peer with three
  corrupt strikes.
- **Tracker numbers are whole-torrent.** `left` counts every unverified piece, and the
  `completed` announce fires only when `have.all()`; selective downloads still emit the local
  UI completion event.
- **Uploaded bytes are credited when queued and refunded when dropped** (choke, eviction,
  closed writer). Any new path that discards queued `Block` frames must do the same.
- **Inbound handshakes get `ACCEPT_HEADROOM`** before capacity eviction; never evict an
  established peer for a connection that has not completed its handshake.
- **Closing the window is a shutdown sequence**, not an exit: stop mpv, send
  `Command::Shutdown`, wait for the supervisor's `Event::Closed` (session fsync + tracker
  `stopped`), then call `iced::exit`.
- **The loopback control API is same-origin guarded, not token-only.** A local app works
  without a token by design; `POST /rpc` still requires `application/json`, a loopback
  `Host`, and a matching `Origin` (when present), which blocks browser CSRF and DNS rebinding.
  Any non-loopback bind refuses to start without an explicit token. Local processes can still
  call the API — that is the accepted single-user desktop model.
- **Disk backpressure.** `DiskWriter::try_write_block` returns `BlockRejected::Full` with
  the payload; the swarm holds it in a bounded pending list and stops requesting.
- **Peer frame cap** is 131 081 bytes, rejected from the 4-byte prefix. `PeerConfig::pieces
  = 0` means "unknown" (metadata fetch): bitfields are acknowledged undecoded.
- **Refuse, don't approximate.** `Priority::Low` has no engine equivalent (returns
  `Unsupported`); v2-only magnets and torrents are refused because Merkle verification is
  missing.
- **Daemon runtime** is built with `enable_all()`; without a reactor the API panics at bind
  (`cli_runtime_has_io_enabled` guards this).
- **Paths** apply Windows rules on every OS and are canonicalized under the root after
  preallocation, which catches symlink/junction escapes.

## Testing conventions

- No on-disk fixtures: torrents are built with `bencode::encode` + real SHA-1/SHA-256.
- No sleeps for ordering: drive commands/events, pass `now` into trackers and limiters.
- `tests/e2e.rs` proves the real paths: direct peer, HTTP tracker discovery, magnet via
  tracker, selective download, restart persistence.
- Security tests that must stay green: path-traversal corpus, hostile bencode, private
  flag propagation to DHT, piece-straddling selective download.

## Next work, in dependency order

1. MSE/PE encryption (private trackers, DPI) and wiring the existing `RateLimiter` into the
   peer writer with global/per-torrent caps and LAN exemption.
2. uTP/LEDBAT; PEX + LSD (PEX needs a BEP 10 handshake in normal swarms); UPnP/NAT-PMP/PCP;
   IPv6 listener.
3. v2 Merkle verification; web seeds and scrape.
4. Add sheet with file tree before adding; per-torrent save path; share limits and RSS/watch
   folder.
5. Windows Authenticode via SignPath Foundation or Azure Trusted Signing (workflow + docs are
   ready; an account/approval is required), macOS validation, virtualised table.
6. Native `io_uring`/IOCP backend behind `BlockBackend`.
