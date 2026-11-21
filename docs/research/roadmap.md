# Oxide Flux — consolidated gap roadmap

Date: 2026-11-21. Sources (all verified against code by separate audits):

- [`gaps-protocol.md`](gaps-protocol.md) — BEP coverage, peer-wire correctness, discovery, traffic.
- [`gaps-engine.md`](gaps-engine.md) — engine/product features, lifecycle truth, API surface.
- [`gaps-ui.md`](gaps-ui.md) — implementation vs `docs/design/ui-spec.md`, playback, a11y.
- [`gaps-quality.md`](gaps-quality.md) — security, testing, ops, distribution, licensing.

Baseline: the client transfers over TCP, announces to HTTP/UDP trackers, uses IPv4 DHT,
fetches magnets via `ut_metadata`, seeds, persists sessions, exposes JSON-RPC/WebSocket, and
has a native iced shell with per-user `.torrent`/`magnet:` associations.

## Already fixed while reading the reports

| Item | Fix | Evidence |
| --- | --- | --- |
| CSRF / DNS rebinding against the loopback API | Same-origin `Origin`/`Host` gate, loopback host allow-list, `application/json` required (forces preflight), WebSocket covered | `flux-api/src/http.rs` (`request_is_local`), `ws.rs`, tests in `tests/http.rs` |
| Symlink/junction resized before rejection | All ancestors and existing files are link-checked and canonically confined **before** the writer opens or grows anything; deletion re-checks and never removes through a link | `flux-daemon/src/verify.rs` (`prepare_paths`, `delete_files`), Unix symlink + Windows junction tests |
| Block provenance / poisoning | `Picker::on_block_received` only accepts a block from a peer that holds the request; corrupt pieces record contributors and ban a sole contributor (or a peer with 3 strikes) | `flux-core/src/picker.rs` (`BlockNotRequested`, `requeue_block`), `swarm.rs` (`piece_sources`, `banned`) |
| Inbound eviction before handshake | Newcomers get an `ACCEPT_HEADROOM` handshake window; eviction happens only in `on_connected` after the handshake succeeds | `swarm.rs` (`start_accept`, `accepting`) |
| Stale-request sweep | Each request carries its own issue time; only blocks older than 60 s are requeued, via `Picker::requeue_block` | `swarm.rs` (`PeerState::requested`, `requeue_stale_requests`) |
| Selective-download announce truth | `left` is whole-torrent remaining bytes; tracker `completed` fires only when every piece is verified (local completion still emits the UI event) | `swarm.rs` (`left_bytes`, `tracker_completed`), `tests/e2e.rs` |
| Upload accounting | Blocks credit `uploaded` when queued and are refunded when choke/eviction/closed writer discards them | `swarm.rs` (`block_len`, `discard_pending_blocks`) |
| Graceful close | Window close sends `Command::Shutdown`, stops mpv, waits for the supervisor's `Event::Closed` (session fsync + tracker `stopped`), then exits | `flux-app/src/app.rs` (`closing`, `close_event`), `tests/supervisor.rs` |

## Phase 0 — correctness before features

Status: all seven items are implemented with tests (see the table above). One follow-up only:

1. **Poison-ban regression test and visibility** (P1, S). The current rule bans a sole
   contributor or a peer with three corrupt strikes; expose a banned-peer count in
   `SwarmSnapshot` and assert that a malicious seeder is disconnected and never re-dialled.

Decided: the loopback API keeps the same-origin-only model (no token by default, JSON +
Host/Origin required, token mandatory on non-loopback binds). See `AGENTS.md`.

## Phase 1 — streaming control plane + truthful UI

This is what makes it a *streaming* client rather than a downloader.

1. Playhead/sequential API: expose `set_playhead`, mode switch (sequential/rarest), buffered
   range, per-file verified bytes. `SwarmHandle::set_playhead` already exists; nothing calls it.
2. Adapter truth: map `Active→Downloading`, `Seeding→Seeding`, count `stats.downloading`,
   surface `lastError`; never gate Play on partial data without a contiguous window.
3. UI truth pass: remove "Sequential streaming ready", `—` for unknown size/ETA/ratio, state
   colours, completion-gated Play (`gaps-ui.md` G01).
4. Flush pane layout + 2 px selection bars; table shell (separators, right-aligned numerics,
   ellipsis, hover menu, double-click).
5. Keyboard/focus chain: shortcuts, `focus_next/previous`, visible 2 px rings.
6. Embedded mpv child window (`--wid`, HWND via `window::run` + `raw-window-handle`, host-driven
   resize/DPI/focus, `--input-ipc-server` for controls) + LICENSE decision.

## Phase 2 — transport completeness

1. **MSE/PE** — private trackers and DPI-shaped networks; currently every peer connection is
   plaintext. Largest single compatibility gap.
2. **Rate limiting wired in** — `RateLimiter` exists but the peer writer never uses it; add
   global/per-torrent caps, payload-only accounting, LAN exemption, alt-speed.
3. **uTP/LEDBAT** — latency-friendly transport.
4. **PEX + LSD**; PEX first requires swarms to send a BEP 10 handshake at all (today only the
   metadata fetch does).
5. **Inbound reachability** — UPnP, NAT-PMP/PCP, holepunch; IPv6 listener (DHT is IPv4-only).
6. **v2 Merkle verification**, then hybrid downloads; web seeds and scrape.

## Phase 3 — product parity

- Add sheet with file tree and priorities before adding; per-torrent save path.
- Settings screen (network, bandwidth, appearance, associations) + `ui-state.json`.
- Peer list, tracker list/status, per-file progress in API/UI.
- Categories/tags, queue top/bottom, force start, rename/set-location/export.
- Share limits (ratio/time), free-space guard, watch folder, RSS filters, scheduler.
- Table windowing (`VirtualTable`) and 4 Hz snapshot policy for 10k torrents.

## Phase 4 — ship

- Decide the project license (mpv is GPL; fonts OFL, icons ISC) and add LICENSE/third-party
  notices.
- Per-user Windows installer, GUI-subsystem binary, icon + manifest, uninstall removing
  associations/state; code signing; tag-driven release with checksums/SBOM.
- Structured logging with secret redaction, panic hook, actionable disk-full/permission errors.
- Config/state schema versions + migrations + last-good backup; corrupt-state quarantine.
- Test gates: fuzz (bencode/metainfo/tracker/DHT/peer/magnet), real-client interop
  (qBittorrent/libtorrent/Transmission), soak, 500+ Mbit/s disk benchmark, macOS CI.
- Auto-update: decide update channel and signature verification before 1.0.

## Suggested order by user value

`Phase 0 (API auth decision, poison test)` → `Phase 1 (1–4)` → embedded mpv → `Phase 2 (1,2)` →
settings/add sheet → PEX/LSD/NAT → Phase 3/4.

Rationale: the correctness and streaming-control work already landed; the remaining blockers
are the API auth decision and the streaming UI/control plane, then MSE and rate limiting.
