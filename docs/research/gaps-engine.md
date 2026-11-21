# Engine & control-plane gaps — Oxide Flux vs qBittorrent 5.x / FDM 6.x

Status: research, read-only. Date: 2026-11-21. Scope: `flux-daemon`, `flux-api`, `flux-core`,
`flux-net`; UI expectations taken from `docs/design/ui-spec.md` and the shipped WebUI
(`flux-api/static/app.js`). Not a git repo: all line numbers were read from the working tree
and may move. `State`: **done / partial / absent**. `P0` = blocks honest day-to-day use or
makes the current UI lie; `P1` = expected by most qB/FDM users; `P2` = nice to have.
Inferences are marked **(inf.)**. Evidence key: bare `engine.rs`/`methods.rs`/`ws.rs`/`http.rs`
are `flux-api/src/`; `session.rs`/`swarm.rs`/`state.rs`/`config.rs`/`announce.rs`/`verify.rs`/
`pieces.rs`/`magnet.rs` are `flux-daemon/src/`; `picker.rs`/`path.rs` are `flux-core/src/`;
`peer.rs`/`limit.rs`/`tracker/mod.rs` are `flux-net/src/`.

## Verdict

- The engine really transfers, but the control plane still looks like a storage/verify tool:
  **9 RPC methods** (`flux-api/src/methods.rs:19-35`) and an 11-field summary
  (`flux-api/src/engine.rs:88-111`).
- Three truth defects: every active download is reported as `queued`, every seed as
  `complete`, and `session.stats.downloading` is never incremented
  (`flux-api/src/engine.rs:217-224`, `176-186`). The WebUI renders exactly those fields
  (`flux-api/static/app.js:116-117`, `151`).
- Failures and warnings are invisible: `TorrentSummary` has no `lastError`, though the session
  tracks it (`flux-daemon/src/session.rs:235-236`), and `Event::Warning` is dropped by the
  bridge (`flux-daemon/src/engine.rs:302`).
- Whole feature families are absent: categories/tags, queue control, bandwidth limits, share
  limits, watch folder, RSS, scheduler, peer/tracker views, preferences, logs, TLS.
- Some capability already exists but is unreachable: `recheck`, `piece_state`, `add_peer`
  (`session.rs:474-632`) and the streaming playhead (`swarm.rs:358-368`).
- Cheapest wins are adapter/DTO work, not new wire protocol.

## 1. Library management

| Feature | Expected by | State | Evidence | Gap → impact | Effort | P |
|---|---|---|---|---|---|---|
| Save path per torrent | qB, FDM | partial | `state.rs:74` stores `download_dir`, but `session.rs:1639` always writes `None`; prepare and swarm use the global root (`session.rs:1367-1370`, `swarm.rs:1682`) | Field is dead; no `savePath` on add or set-location → users with multiple drives cannot place torrents | M | P0 |
| Categories / tags | qB | absent | No field in `state.rs:63-75`; no method in `methods.rs:19-35` | No grouping/filtering at 100+ torrents **(inf.)** | M | P1 |
| Add paused | qB, FDM | absent | `AddTorrent` has only `kind/bytes/uri` (`engine.rs:171-184`) | Add always preallocates and starts; workaround is a second `torrent.pause` call (`session.rs:1492`) | S | P1 |
| Queue priority top/bottom | qB | absent | Slot order is add order via immutable `seq` (`session.rs:752`, `1883-1930`); queue position is computed (`session.rs:2152-2161`) but not in the DTO | Cannot reorder; long queue cannot be steered | S | P1 |
| Force start | qB | absent | Eligibility always requires a free slot (`session.rs:2170-2176`); no flag in `Torrent` | A large queue blocks an urgent torrent | S | P1 |
| Recheck | qB | partial | `SessionHandle::recheck` exists (`session.rs:629-632`); no RPC method (`methods.rs:19-35`) | UI cannot trigger the existing path; only restart/CLI verify | S | P1 |
| Reannounce | qB | absent | `TrackerService::announce_now` exists (`announce.rs:245-255`) but has no caller outside tests; tracker lives inside the swarm | Cannot retry a stuck tracker from the UI | S-M | P1 |
| Sequential flag / first-last pieces | qB | partial | Picker supports `Sequential`/`RarestFirst` (`picker.rs:32-40`), but the swarm hardcodes `Mode::Sequential` (`swarm.rs:648`); no first/last-piece option in `Picker` | No user toggle; no preview-oriented first/last ordering | S-M | P1 |
| Duplicate detection | qB, FDM | done | Rejected in `session.rs:1398-1403`; mapped to `-32002` (`engine.rs:169-172`) | — | — | — |
| Export `.torrent` | qB | absent | Raw bytes retained (`verify.rs:78`) but no DTO/method | Cannot share or back up a torrent | S-M | P2 |
| Rename torrent | qB | absent | Name is read from the plan (`session.rs:878`); no override field | Cosmetic; wrong names persist | M | P2 |
| Set location / move data | qB | absent | Root fixed at add (`session.rs:1368`); no move code | Users must remove+re-add and move files manually | L | P2 |
| Super seeding | qB | absent | No implementation or flag | Rare; only for initial-seed ratios | L | P2 |

## 2. Automation

| Feature | Expected by | State | Evidence | Gap → impact | Effort | P |
|---|---|---|---|---|---|---|
| Watch folder | qB, FDM | absent | No watcher module; only explicit add calls (`session.rs:501-529`) | Manual adds only | M | P1 |
| RSS feeds + regex filters + auto-download | qB, FDM | absent | No RSS/feed type or module in `flux-daemon/src` | Core FDM workflow missing | L | P1 |
| Scheduled speed/queue | qB, FDM | absent | `Config` has no time rules (`config.rs:94-125`) | Limits (once added) cannot follow a schedule | M | P2 |
| Ratio/time share limits, per torrent/global | qB | absent | No fields on `Torrent` (`session.rs:751-773`) or config; completion never auto-pauses | Client seeds forever or must be stopped by hand **(inf.)** | M | P1 |
| Free-space guard | qB, FDM | absent | No capacity probe; only an IO error after allocation fails (`verify.rs:658-665`) | Can fill the disk and fail every active torrent | M | P1 |

## 3. Traffic & limits

| Feature | Expected by | State | Evidence | Gap → impact | Effort | P |
|---|---|---|---|---|---|---|
| Global down/up limits | qB, FDM | absent | `RateLimiter` exists (`flux-net/src/limit.rs:18-20`, `53-180`) but has no caller; `Config` has no rate fields (`config.rs:94-125`); session has no limiter (`session.rs:1017-1037`) | Client can saturate the link; no user control | M | P0 |
| Per-torrent limits | qB | absent | `SwarmConfig` has peers/slots but no rates (`swarm.rs:160-183`) | Cannot cap one torrent | M | P0 |
| Alternate speed mode | qB | absent | No alt-speed flag anywhere (`config.rs`, `swarm.rs`) | No "turtle mode" | M | P1 |
| LAN exemption | qB | partial | `TrafficClass::Local` is defined and skipped (`limit.rs:24-45`), never used; peer address is available (`swarm.rs:493`) | Once limits exist, LAN traffic would be throttled without the classification | S-M | P1 |
| Protocol-overhead accounting | qB (payload vs total) | partial | Counters add block payload only (`swarm.rs:1307-1308`, `1836`); no framed-byte counter | UI cannot show real wire usage **(inf.)** | S | P2 |
| Connection limits | qB | partial | Per-torrent cap done (`config.rs:105-108`, `swarm.rs:1020-1035`); no global counter in session | Many torrents can exceed the router's NAT table **(inf.)** | S | P1 |
| Proxy (HTTP/SOCKS) | qB | absent | No proxy config or client support anywhere in `crates/` | Tracker/DHT/peer traffic cannot be tunneled | L | P2 |

## 4. Seeding / serving

| Feature | Expected by | State | Evidence | Gap → impact | Effort | P |
|---|---|---|---|---|---|---|
| Tit-for-tat choking | qB | done (basic) | 10 s rechoke ranks by bytes since tick (`swarm.rs:1882-1928`); rate-based, no reciprocation history or snub | Works; less fair under bursty peers **(inf.)** | — | — |
| Optimistic unchoke | qB | done | Every 3rd tick replaces the last slot with a rotating peer (`swarm.rs:1899-1916`) | — | — | — |
| Upload slots / seed boost | qB | done | `max_upload_slots`, seeds get `max(2)` (`swarm.rs:1892-1896`); immediate unchoke when free (`1930-1943`) | — | — | — |
| Seeding while downloading | qB | done | Uploads check `have(piece)`, not completion (`swarm.rs:1768`) | — | — | — |
| `completed` event semantics | qB | partial | Sent once on desired-set completion (`swarm.rs:1440-1452`); session discards `SwarmEvent::Completed` (`session.rs:1797`); no API event | Tracker semantics are defensible, but clients get no completion notification | S | P2 |
| "Seeding" status accuracy (selective) | qB | partial | Session requires every desired piece (`session.rs:811-816`, good), but the API maps `Seeding → Complete` (`engine.rs:222`) and progress is `have/all pieces` (`engine.rs:200-204`) | A selective seed displays "complete" at e.g. 60% progress and never "seeding" | S | P1 |

## 5. API surface (`flux-api`)

| Need | State | Evidence | Missing from the DTO / trait | Impact | Effort | P |
|---|---|---|---|---|---|---|
| Lifecycle truth | partial | `engine.rs:217-224` maps `Active→Queued`, `Seeding→Complete`; `stats.downloading` never set (`176-186`) | No `downloading`/`seeding`/`error` state reaches a client | Table and counters contradict each other; UI looks stalled | S | P0 |
| Failure visibility | absent | `TorrentSummary` (`88-111`) has no error; session has `last_error` (`session.rs:235-236`); `Warning` dropped (`engine.rs:302`) | No `lastError`, `verification`, `piecesCorrupt`, warnings, or log | User cannot diagnose a stalled torrent | S | P0 |
| Per-file progress | absent | `FileView` only `index/path/length/priority` (`engine.rs:116-125`); session computes `completed`/`bytes_completed` (`session.rs:175-177`, `862-871`) | `bytesCompleted`, `progress`, `selected` | File tree cannot show progress; streaming UI impossible | S | P0 |
| Peer list | absent | `PeerEvent` carries no rates/client (`peer.rs:280-329`); `PeerState` has addr/flags only (`swarm.rs:492-520`) | `peers[]`: addr, client, flags, progress, down/up, requests | No diagnostics, no ban/limit decisions **(inf.)** | M | P1 |
| Tracker list/status | absent | `AnnounceResponse` has interval/complete/incomplete/warning (`tracker/mod.rs:132-146`); swarm discards everything except peers (`swarm.rs:816-828`) | `trackers[]`: url, tier, status, next announce, seeders/leechers | Cannot see why discovery fails | M | P1 |
| Core metrics | partial | Summary has rates and peers (`engine.rs:104-110`); no ETA/ratio/totals/timestamps | `eta`, `ratio`, `amountLeft`, `downloaded/uploaded`, `addedOn`, `seeds/leechers` | UI must fake ETA/ratio or omit them **(inf.)** | S | P1 |
| Preferences get/set | absent | `Config` is immutable `Arc` (`session.rs:403`); no method (`methods.rs:19-35`) | Any settings RPC | Limits/queue settings need a restart | M | P1 |
| Session logs | absent | Only `Event::Warning` (`session.rs:314-315`), dropped (`engine.rs:302`) | `session.log` ring buffer / `log` events | No support trail | S-M | P1 |
| Events | partial | 3 notifications (`ws.rs:34-44`); checking ticks filtered (`engine.rs:301`) | completed, error, tracker, queue, log events | Clients poll; failures never push | S-M | P1 |
| Auth / TLS / remote | partial | Bearer only, constant-time (`http.rs:260-288`); non-loopback needs a token (`99-101`); plain TCP (`serve`, `lib.rs:210`) | TLS, users, per-token scopes | Remote access needs a reverse proxy | M | P2 |
| API versioning | partial | `api_version = 1` constant (`engine.rs:26-32`), no capability list | Feature negotiation | Clients must hard-code version | S | P2 |

Method-level: `torrent.setFilePriorities` rejects `low` by design
(`flux-daemon/src/engine.rs:149-153`), and no preferences/export/recheck/reannounce methods
exist (`methods.rs:19-35`).

## 6. Session lifecycle

| Feature | State | Evidence | Gap → impact | Effort | P |
|---|---|---|---|---|---|
| Resume | partial | State restores metainfo/priorities/paused through the normal add path (`session.rs:1168-1175`, `1413-1427`); no piece bitfield is stored (`state.rs:63-75`), so startup rehashes everything (`session.rs:1950-1969`, `2089-2134`) | Correct but O(size) every launch; slow for 100 GB torrents | M | P1 |
| State versioning / migration | partial | Only version 1 accepted; any other version is a hard error (`state.rs:244-250`); no migration | A format bump bricks startup until the file is deleted | S-M | P1 |
| Crash recovery | done | Atomic temp+fsync+rename (`state.rs:261-300`); writer flushes every 500 ms (`swarm.rs:154`); startup re-verifies | At most ~500 ms of blocks lost, truthfully re-derived | — | — |
| Shutdown flush | done | Shutdown stops swarms before persisting (`session.rs:1309-1322`); writer/verifier/tracker shutdown in `swarm.rs:2036-2044`; tracker `stopped` (`announce.rs:274-278`) | — | — | — |
| Single instance | partial | Desktop has lock+queue (`flux-app/src/single_instance.rs:14`); the daemon has none | Two `fluxd` on one state file: last atomic rename wins, torrents silently vanish **(inf.)** | S | P1 |
| Config hot reload | absent | `Arc<Config>` built once (`session.rs:403`); `daemon_command` receives a parsed config and never re-reads it (`cli.rs:485-498`) | Any setting change needs a restart | M | P1 |
| Multi-user paths | absent | One bearer token (`http.rs:44-52`); state file inside the download dir (`state.rs:167-169`) | No per-user libraries or permissions | L | P2 |
| Add latency | partial | `torrent.add` waits for preallocation (`session.rs:1364-1382`) and for magnet metadata (`session.rs:444-458`, 60 s default `magnet.rs:88`) | HTTP add can time out; no "metadata pending" object | M | P1 |

## 7. Streaming support

| Feature | State | Evidence | Gap → impact | Effort | P |
|---|---|---|---|---|---|
| Sequential mode exposure | partial | Picker supports two modes (`picker.rs:32-40`); swarm hardcodes `Sequential` (`swarm.rs:648`); desktop shows a static "Sequential streaming ready" label (`flux-app/src/app.rs:926`) | No toggle or per-torrent policy | S | P1 |
| Playhead API | absent | `SwarmHandle::set_playhead` exists (`swarm.rs:358-368`, handled `916-919`); `SessionHandle` has no command (`session.rs:474-641`); no RPC | Picker always streams from piece 0, not from what mpv is reading | S | P0 |
| Buffering window | partial | Lookahead defaults to 8 pieces (`picker.rs:24`) and is never configured; no buffer state in DTOs | No read-ahead control or "buffered" reporting | S-M | P1 |
| Per-file progress | absent | Same work item as §5 (`session.rs:862-871`); no DTO field | "Play while downloading" cannot decide readiness | S | P0† |
| Play while downloading | partial | Incomplete pieces are flushed to disk every 500 ms (`swarm.rs:154`, `1457-1481`); the app plays any listed file immediately through external mpv (`app.rs:384-389`, `1152-1174`) | Bytes exist but nothing tells the UI which ranges are verified; mpv can read holes | M | P1 |
| Media probing | absent | No container/duration probe; `player::is_media` is a name check used only by the app (`app.rs:1152`) | No duration, no resume position, no codec warning | M | P2 |
| Piece-state API | partial | `SessionHandle::piece_state` (`session.rs:615-619`) but no RPC | UI cannot draw a buffer map | S | P2 |

† Same work item as the §5 per-file progress row; not double-counted in the P0 total.

## Top 8 missing capabilities (value order)

1. **Streaming control plane** — playhead command, per-file verified progress, buffer window,
   sequential/rarest toggle. The picker and `set_playhead` already exist; this is wiring.
2. **Truthful lifecycle and failure reporting** — real `downloading`/`seeding`/`error`,
   `stats.downloading`, `lastError`, warnings as events/log lines.
3. **Per-torrent save path + add options** — `savePath`, `paused`, `sequential` on add; state,
   prepare, swarm and DTO must all carry the root.
4. **Bandwidth control** — global and per-torrent up/down limits, alt-speed, LAN exemption.
   `RateLimiter` exists unused; integrate, don't rewrite.
5. **Queue and library commands** — top/bottom, force start, recheck, reannounce, rename,
   set-location, export.
6. **Peer and tracker visibility** — per-peer list/speeds/flags and per-tracker status with
   next-announce and seeder/leecher counts. Requires new swarm snapshots, not new protocol.
7. **Share limits and free-space guard** — ratio/time auto-stop per torrent and globally; refuse
   or pause when the target volume is below a threshold.
8. **Automation** — watch folder, RSS with regex filters and auto-download, time-based scheduler.

## Sequencing proposal

- **Phase A — tell the truth (S, days).** Fix `api_state`, `Stats.downloading`, add
  `lastError/verification/queuePosition/bytes*` to the summary, per-file progress, warnings and
  errors as events, expose existing `recheck`/`pieceState`. No protocol risk; unblocks every UI
  screen and the streaming work.
- **Phase B — steer the transfer (M, 1–2 weeks).** Playhead/sequential/buffer API; add options
  and per-torrent roots through verify/state/swarm; queue top/bottom and force start; wire
  `RateLimiter` for global/per-torrent limits plus alt-speed; `preferences.get/set`.
- **Phase C — operate at scale (M).** Share limits and free-space guard; peer/tracker DTOs;
  session log ring buffer; finished/error/tracker events; global connection cap.
- **Phase D — breadth (L).** Watch folder, RSS filters, scheduler, rename/set-location/export,
  TLS or documented reverse-proxy remote access, proxy support. (MSE/uTP/PEX/LSD/web seeds stay
  on the networking roadmap and are out of scope here.)

Phases A+B unlock the "streaming-first" promise with no new peer-wire work and are the highest
value per week; C makes it usable for heavy seeders; D broadens the audience.

## Cheapest high-impact wins (do these first)

1. **Adapter truth fix** — `flux-api/src/engine.rs:217-224` and `176-186`: map `Active →
   Downloading`, `Seeding → Seeding`, count `downloading`, surface `last_error`. One file, S.
2. **Per-file progress + expose `recheck`** — add `bytesCompleted/completed` to `FileView` and
   two RPC methods around existing `session.rs:615-632`. S.
3. **Playhead/sequential API** — pass `set_playhead` (`swarm.rs:358`) through a session command
   and one RPC method; this is the difference between "downloads in order" and "streams".

## Already good — don't redo

- Strict bencode/metainfo parsing and hostile-input tests (`flux-core/src/bencode.rs`,
  `metainfo.rs`); path rules plus canonical root check (`flux-core/src/path.rs`,
  `verify.rs:350-375`).
- Atomic, fsynced state saves with strict corruption errors (`state.rs:261-300`, `244-250`).
- Truthful startup verification (no bitfield trust) (`session.rs:1168-1175`, `2089-2134`).
- Selective-download geometry, including pieces straddling skipped files (`pieces.rs`,
  `AGENTS.md`).
- Add-order queue slots and promotion (`session.rs:1883-1930`).
- Picker endgame, cancels and stale-request requeue (`picker.rs:488-590`,
  `swarm.rs:1676-1695`); disk backpressure (`swarm.rs:555`, `887`).
- Choking, optimistic unchoke, upload slots and seed boost (`swarm.rs:1882-1943`).
- Tracker service with backoff, single `completed`, best-effort `stopped` (`announce.rs`).
- DHT privacy enforced twice (`session.rs:1842`, `1864-1871`); magnet metadata hash check
  (`magnet.rs`).
- Bearer auth with constant-time compare and non-loopback guard (`http.rs:260-288`, `99-101`).
- Duplicate rejection and typed engine errors (`session.rs:1398-1403`, `engine.rs:163-189`).

## Notes / caveats

- `flux-daemon/src/lib.rs:5-7` and `cli.rs:469-478` still claim there is no peer layer or tracker
  traffic; the code disproves both. Stale docs mislead contributors (not a runtime gap).
- "Seeding accuracy with selective downloads" is a display/DTO problem, not a verification
  problem: `complete()` correctly means every *desired* piece (`session.rs:811-816`).
- Effort sizes assume one engineer familiar with the actor/swarm split; protocol additions
  (MSE, uTP, PEX, v2 Merkle, web seeds) are excluded from this report.
