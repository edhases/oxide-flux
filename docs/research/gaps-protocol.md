# Oxide Flux — Protocol & Network Gap Audit

**Date:** 2026-11-21 · **Scope:** `flux-net`, `flux-daemon`, `flux-core` protocol paths;
`flux-api`/`flux-app` only where they gate streaming. **Method:** static read of the
workspace at `E:\Github\Oxide Flux`; no source changed, no runtime/live-swarm tests run.
Line numbers are from this tree. Sources: `AGENTS.md` (brief + "Next work"), the BEPs, and
`docs/design/ui-spec.md` §7.4/§11. "Parity" means a 2026 competitive baseline with no
explicit project text. Paths are abbreviated: `peer.rs`/`message.rs`/`handshake.rs`/`dht.rs`/
`limit.rs`/`metadata.rs`/`tracker/*` are under `crates/flux-net/src/`; `picker.rs`/`metainfo.rs`/
`core/magnet.rs` under `crates/flux-core/src/`; `swarm.rs`/`session.rs`/`announce.rs`/
`config.rs`/`verify.rs`/`magnet.rs` under `crates/flux-daemon/src/`; `app.rs` under
`crates/flux-app/src/`.

**Legend:** state = implemented / partial / absent. Effort S ≈ days, M ≈ 1–3 weeks,
L > 3 weeks. Priority P0 blocks a "real client", P1 competitive parity, P2 long tail.
Priority counts cover gaps only; implemented rows are not counted.

## 1. Coverage matrix

### 1.1 BEP coverage

| # | Feature | Source | State | Evidence (path:line) | Gap / user-visible impact | Effort | Prio | Depends |
|---|---|---|---|---|---|---|---|---|
| A1 | BEP 3 peer wire (handshake, 10 msgs, keepalive) | BEP 3; AGENTS:5 | implemented | `peer.rs:465-516`, `message.rs:49-70`, `peer.rs:707-785` | None for base wire. Fast/unknown ids 13–19 are fatal (`message.rs:597`), so only a peer that ignores negotiation is dropped. | — | — | — |
| A2 | BEP 5 DHT (IPv4) | BEP 5; AGENTS:5 | partial | `dht.rs:333-372`, `dht.rs:1505-1542`, `session.rs:1833-1859` | IPv6 nodes absent (`dht.rs:17,334`); no routing-table refresh/age-out; peer `PORT` ignored (`peer.rs:911`). DHT-discovered v6-only swarms are invisible; the table goes stale on long runs. | M | P1 | — |
| A3 | BEP 6 fast extension | BEP 6 | absent | `handshake.rs:66-67` defines bit; `peer.rs:519-524` never sets it; ids 13–19 fatal `message.rs:522-598` | No `have_all`/`have_none`/`reject`; full bitfield frames for big torrents. Low impact while unadvertised. | S | P2 | — |
| A4 | BEP 9 `ut_metadata` (leech) | BEP 9; AGENTS:5 | implemented | `magnet.rs:426-568`, `metadata.rs:140-200`, hash check `magnet.rs:732-745` | None for fetching. | — | — | — |
| A5 | BEP 9 `ut_metadata` (serve) | BEP 9 | absent | `swarm.rs:1289-1291` drops `PeerEvent::Extended`; no `m` map in swarm handshake | Cannot serve metadata to magnet peers; seeding a magnet is impossible. | S/M | P2 | A6 |
| A6 | BEP 10 extended handshake in swarm | BEP 10 | absent | `swarm.rs:1130-1132` only queues bitfield; `swarm.rs:1289-1291` ignores extended | Normal connections never send `m`/`v`/`reqq`/`metadata_size`/`p`; no PEX/ut_metadata/upload_only. Magnet fetch alone sends one (`magnet.rs:596-615`). | M | P1 | — |
| A7 | BEP 11 PEX | BEP 11; AGENTS:6 | absent | no `ut_pex` anywhere; `swarm.rs:76` | Trackerless/private swarms get no peer exchange; discovery depends on tracker/DHT. | M | P1 | A6 |
| A8 | BEP 12 multi-tier announce | BEP 12 | partial | `tracker/mod.rs:550-562`, `575-593` | One tracker per interval; tiers are failover, not parallel BEP 12. | S | P2 | — |
| A9 | BEP 14 LSD | BEP 14; AGENTS:6 | absent | no multicast code; `flux-net/src/lib.rs:9` | No LAN discovery; offline/LAN transfers impossible without tracker/DHT. | S/M | P2 | — |
| A10 | BEP 15 UDP tracker | BEP 15 | partial | `tracker/udp.rs:183-206`, retries `219-268` | Uses only `addresses.first()` (`tracker/udp.rs:190`), so a dual-stack tracker can fail on a v4-only host. Announce itself is solid. | S | P2 | — |
| A11 | BEP 19 web seeds | BEP 19; AGENTS:6 | absent | `core/magnet.rs:71-72` parses `ws` but `FetchSpec` ignores it; no `url-list` in `metainfo.rs` | Dead torrents with HTTP seeds cannot be rescued; archive.org-style torrents fail. | M | P1 | — |
| A12 | BEP 27 private | BEP 27 | partial | `metainfo.rs:806-815`; DHT gating `session.rs:1842,1869-1870`; `swarm.rs:76` | Flag parsed and DHT gated, but no MSE (`E1`) and no PEX/LSD code to gate; encryption-required private trackers still refuse. | M | P1 | E1 |
| A13 | BEP 29 uTP/LEDBAT | BEP 29; AGENTS:6,119 | absent | `flux-net/src/lib.rs:9`; no dep; `config.rs:103` still says "TCP/uTP" | No NAT-friendly transport or LEDBAT fallback; TCP-only behind restrictive NATs. | L | P1 | — |
| A14 | BEP 47 padding files | BEP 47 | absent | `metainfo.rs:853-903` ignores `attr`; no `.pad` handling | `.pad` files are exposed, preallocated and downloaded as real files; wasted space, dirty file list. | S | P1 | — |
| A15 | BEP 52 v2/hybrid | BEP 52 | partial | parse/layout `metainfo.rs:645-716`, layers `988-1050`; `verify.rs:142-146`; `swarm.rs:597-600` | v2-only torrents stored but never transferred (no Merkle verify); hybrid works via v1 only; no v2 handshake routing. | L | P1 | disk verify |
| A16 | BEP 53 magnet v2 | BEP 53 | partial | `core/magnet.rs:21-25` parses btmh; `magnet.rs:167-177` returns `NoInfoHash` for v2-only | v2-only magnets refused (honest, but not usable). | S | P2 | A15 |
| A17 | BEP 55 holepunch | BEP 55 | absent | no `ut_holepunch` code | Symmetric-NAT peers cannot connect; no UDP rendezvous. | L | P2 | D1/D3 |
| A18 | WebSocket trackers | parity | absent | `tracker/mod.rs:16`; `announce.rs:88` filters `ws://` | Browser-oriented trackers unusable. | M | P2 | — |
| A19 | Tracker scrape | parity; AGENTS:121 | absent | `tracker/mod.rs:16` | No seed/leech counts, tracker-health UI or ratio display. | S | P2 | — |
| A20 | BEP 5 `port` message | BEP 5 | absent | `peer.rs:911` drops `Message::Port` | Peers cannot tell us their DHT port; no peer-wire DHT integration. | S | P2 | A2 |

### 1.2 Behavioral correctness

| # | Feature | Source | State | Evidence (path:line) | Gap / user-visible impact | Effort | Prio | Depends |
|---|---|---|---|---|---|---|---|---|
| B1 | Choking 10 s / 30 s optimistic | BEP 3 | implemented | `swarm.rs:143-145`, `1882-1928` | 10 s rechoke, every 3rd tick optimistic. No snub (D6). | — | — | — |
| B2 | Endgame duplicates + cancel batching | BEP 3 | implemented | `picker.rs:475-480,610-632`, cancels `752-764`, routing `swarm.rs:1661-1674` | Cap 32 cancels per drain; a dropped cancel costs one duplicate block. | — | — | — |
| B3 | Request timeouts | BEP 3 | partial | `swarm.rs:139` (60 s), `1676-1695`; oldest-only `515,1597-1606` | One slow request ages the whole peer; all requests requeued without `cancel`; up to 60 s stalls and duplicate requests. | S/M | P1 | B4 |
| B4 | Peer poisoning / corrupt attribution | BEP 3; security | absent | `picker.rs:688-726` validates availability/geometry, not in-flight; `swarm.rs:1303-1329`; corrupt path `1423-1428` drops source | Any advertised piece can be filled with unsolicited data; a poisoner is never identified or banned, so the last piece can loop forever. | M | P0 | — |
| B5 | Sequential/rarest/streaming playhead | brief; ui-spec:1038-1050,1688 | partial | `picker.rs:636-655` both modes; `swarm.rs:648` forces Sequential; `swarm.rs:358-368` `set_playhead` has no caller | No stream mode, playhead movement or buffered ranges; Play is offered on incomplete files (`app.rs:1171-1181`), so mpv gets a partial file. | M | P1 | API DTO |
| B6 | Rarest-first selection | parity | partial | `picker.rs:645-654`; never selected (`swarm.rs:648`) | Bulk downloads use sequential only; swarm health suffers on rare pieces. | S | P2 | B5 |
| B7 | Selective piece straddling | AGENTS:71-72; test | implemented | `pieces.rs:245-259`; `e2e.rs:278-338` | Straddling piece fetched whole; skipped side written but not otherwise requested. | — | — | — |
| B8 | DHT privacy for private | AGENTS:69-70 | implemented | `session.rs:1842`, `1869-1870` | Announce and ingest both skip private. A trackerless private magnet can query DHT before metadata (`session.rs:449-456`), unavoidable then. | — | — | — |
| B9 | Tracker failure/backoff | BEP 3; AGENTS | partial | `tracker/mod.rs:342-350`; `announce.rs:446-466`; failures dropped `swarm.rs:822` | Backoff works, but the UI never sees tracker errors and there is no reannounce API. | S | P2 | — |
| B10 | DHT bucket eviction | BEP 5 | partial | `dht.rs:1505-1542`, `1090-1155` | LRS ping/evict is correct; no periodic refresh or age-out, so dead nodes linger until a bucket fills. | M | P2 | A2 |
| B11 | Inbound routing by info hash | AGENTS:59-61 | implemented | `session.rs:1059-1091`, `1810-1829`; `peer.rs:499-516` | v1 hash only; v2 handshakes dropped (A15). | — | — | — |
| B12 | Keepalive / idle timeout | BEP 3 | implemented | `peer.rs:121-124`, `707-785` | 120 s keepalive, 150 s idle; handshake bounded by idle. | — | — | — |
| B13 | Inbound admission / limits | security | partial | `swarm.rs:1059-1064` evicts before handshake; `session.rs:1071-1089` unbounded task per socket | A burst of idle SYNs can evict productive peers; 30 s handshake tasks are unbounded and not throttled per IP. | S/M | P1 | D5 |
| B14 | Tracker `left`/`completed` for selective | BEP 3; BEP 27 | partial (bug) | `swarm.rs:2003-2010` sends `desired_left_bytes`; `1012-1014`; `announce.rs:428-432`; magnet `left: 0` `magnet.rs:347` | A partially selected torrent reports `left=0` + `completed`, so trackers see a seeder; ratio/seedbonus and private rules are wrong. | S | P1 | — |
| B15 | Upload request queue | BEP 3 | partial | `swarm.rs:1781-1790` queues 64 then silently drops; `1762-1764` rejects >128 KiB | A dropped request is not rejected on the wire; the peer waits for its own timeout. | S | P2 | — |
| B16 | Upload byte accounting | BEP 3 | partial (bug) | `swarm.rs:1832-1837` counts when queued; `choke` discards queued blocks `1961-1963` | Tracker `uploaded`/ratio can exceed bytes actually sent. | S | P2 | — |

### 1.3 Traffic control

| # | Feature | Source | State | Evidence (path:line) | Gap / user-visible impact | Effort | Prio | Depends |
|---|---|---|---|---|---|---|---|---|
| C1 | Rate limiter wiring | `limit.rs` contract; AGENTS:44 | absent | `limit.rs:1-20`; no `RateLimiter` in `swarm.rs`/`peer.rs`; only tests reference it | No cap at all: a fast peer or many torrents saturate the link; upload can starve download ACKs. | M | P1 | — |
| C2 | Payload-only accounting | `limit.rs` contract | partial | `message.rs:204-216`; `limit.rs:26-45`; unused by transport | Contract is correct but never exercised; swarm counters are payload-only (`swarm.rs:1305,1836`). | S | P2 | C1 |
| C3 | LAN exemption | `limit.rs:7-8` | absent | `limit.rs:35-45`; no caller classifies `Local` | When C1 lands, LAN peers would be capped unless classified; today no limiter, so moot. | S | P2 | C1 |
| C4 | Per-torrent / global caps | parity | absent | `config.rs:94-125` has no rate fields; `swarm.rs` has no cap state | No user control; multi-torrent sessions can consume the whole uplink. | M | P1 | C1 |
| C5 | Interface / VPN binding | parity | absent | listener `session.rs:1047` binds 0.0.0.0; DHT `session.rs:1131-1133`; no `socket2` dep | No way to bind to a VPN interface; split-tunnel users leak, with no guard against it. No multicast socket exists to bind either (A9). | M | P1 | — |
| C6 | Proxy support | parity | absent | `tracker/http.rs:97-102` no proxy; peer/DHT/UDP have no proxy path | HTTP trackers may inherit env proxy via reqwest, but peer wire and DHT bypass it. | M | P2 | — |

### 1.4 Connectivity

| # | Feature | Source | State | Evidence (path:line) | Gap / user-visible impact | Effort | Prio | Depends |
|---|---|---|---|---|---|---|---|---|
| D1 | UPnP / NAT-PMP / PCP | AGENTS:119; parity | absent | no dep/code; listener `session.rs:1041-1054` | No automatic inbound; seeding/ratio only with a manual port forward. | M | P1 | D2 |
| D2 | Listen port fallback | AGENTS:67-68 | implemented | `session.rs:1041-1054`: 10 tries, then ephemeral | Correct; the actual bound port is announced. | — | — | — |
| D3 | IPv6 listener | parity | absent | `session.rs:1047` `Ipv4Addr::UNSPECIFIED`; DHT v4 `dht.rs:334` | No inbound IPv6; v6-only peers cannot connect. | S/M | P2 | — |
| D4 | IPv6 peers | parity | partial | HTTP `peers6` `tracker/http.rs:314-315`; UDP v6 `tracker/udp.rs:403-418`; dial `peer.rs:469` | Outbound v6 works when a tracker supplies it; no v6 DHT, no v6 listener. | M | P2 | A2/D3 |
| D5 | Connection limits | parity; security | partial | `config.rs:106`; `swarm.rs:1027-1035`; inbound `session.rs:1071-1089` | Per-torrent cap only; no global or half-open cap; inbound tasks unbounded. | S/M | P1 | B13 |
| D6 | Slow-peer handling | parity | partial | `swarm.rs:1179-1192` evicts lowest rate only at capacity; `1676-1695` | No snub timer or per-peer rate floor; slow peers occupy slots until pressure. | M | P2 | B3 |

### 1.5 Security-adjacent

| # | Feature | Source | State | Evidence (path:line) | Gap / user-visible impact | Effort | Prio | Depends |
|---|---|---|---|---|---|---|---|---|
| E1 | MSE/PE encryption | AGENTS:6,119 | absent | `handshake.rs:64-69` has no crypto bit; `flux-net/Cargo.toml:7-12` no crypto dep | Encryption-required private trackers and ISP shaping block the client; all peer traffic is plaintext. | L | P0 | — |
| E2 | Peer ID randomization | parity | implemented | `session.rs:1095-1102`: `-OF0100-` + 12 random bytes | Fresh per process; static prefix is expected. | — | — | — |
| E3 | IP filtering | parity; security | absent | `config.rs:94-125` `deny_unknown_fields`; no filter module | No eMule/p2p/CIDR blocklists; cannot exclude bad peers or follow blocklist policy. | M | P1 | — |
| E5 | Magnet URI validation | BEP 9/53 | implemented | `core/magnet.rs:8-35,166-177`; conflicting hashes rejected | Strong: hex/base32 btih, btmh multihash, caps, dedupe. `x.pe` ignored, so peer-only magnets fail. | — | — | — |
| E6 | Tracker crypto params | parity | absent | `tracker/http.rs:221-244` no `supportcrypto`/`requirecrypto` | Trackers cannot see crypto capability; some private trackers filter on it. | S | P2 | E1 |

## 2. Priority counts

- **P0:** 2 — B4 (poisoning), E1 (MSE/PE).
- **P1:** 18 — A2, A6, A7, A11, A12, A13, A14, A15, B3, B5, B13, B14, C1, C4, C5, D1, D5, E3.
- **P2:** 22 — A3, A5, A8, A9, A10, A16, A17, A18, A19, A20, B6, B9, B10, B15, B16, C2, C3, C6, D3, D4, D6, E6.
- **Verified present (not gaps):** 11 — A1, A4, B1, B2, B7, B8, B11, B12, D2, E2, E5.
- Total rows 53; 42 gaps.

## 3. Top 5 correctness risks in current code

1. **Unsolicited blocks are accepted, and corrupt pieces have no author.** `Picker::on_block_received`
   (`picker.rs:688-726`) checks that the peer advertised the piece and that offset/length are
   valid, but never that the block was requested from *that* peer or is still in flight.
   `swarm.rs:1303-1329` writes whatever passes, and `1423-1428` resets the piece on hash failure
   without recording which peers supplied its blocks. A peer that advertises a piece can inject
   data unrequested; a poisoner is never banned, so the last piece can fail forever. I found no
   test for this path. **Fix before feature work.**
2. **Inbound admission churns good peers.** `start_accept` (`swarm.rs:1059-1064`) calls
   `evict_one()` *before* the handshake, so a burst of idle connections can evict productive
   peers; each socket also gets an unbounded task with a 30 s handshake timeout
   (`session.rs:1071-1089`), with no per-IP or global half-open cap.
3. **Request timeouts are per-peer, not per-block.** `STALE_REQUEST_TIMEOUT = 60 s`
   (`swarm.rs:139`) is measured from the *oldest* outstanding request (`515,1597-1606`).
   `requeue_stale_requests` (`1676-1695`) then drops every request for that peer without sending
   `cancel`, so one slow block can stall a piece for a minute and cause duplicate requests.
4. **Tracker `left` lies for selective downloads and magnets.** The swarm publishes
   `desired_left_bytes` (`swarm.rs:2003-2010`) and declares complete when the desired set is
   verified (`1012-1014`); a torrent with skipped files therefore reports `left=0` and
   `event=completed` (`announce.rs:428-432`) while it is not a seeder. Magnet metadata fetches
   also announce `left: 0` (`magnet.rs:347`). Ratio/seedbonus and private-tracker rules break.
5. **Tracker upload bytes can exceed bytes sent.** `on_upload` adds the block length when it is
   queued (`swarm.rs:1832-1837`), but `choke()` discards queued `Block` commands
   (`1961-1963`). The counter is never decremented, so reported upload/ratio is inflated.

## 4. Top 5 missing features by impact

1. **MSE/PE (E1).** The single biggest adoption blocker for private trackers and shaped links;
   also the prerequisite for tracker `supportcrypto` signalling.
2. **Streaming playhead + API (B5).** The product is streaming-first, but `set_playhead` has no
   caller, no mode/position/buffered-range DTO exists, and the app enables Play on incomplete
   files. This is a product-defining gap, not a nicety.
3. **Rate limiting (C1/C4).** The limiter is dead code; with no global or per-torrent caps, a
   handful of torrents can saturate the uplink and starve downloads.
4. **Inbound reachability (D1, D3).** No NAT-PMP/PCP/UPnP and no IPv6 listener; seeding ratio is
   capped by manual port forwarding, and v6-only peers cannot connect back.
5. **PEX + LSD (A7/A9, gated by A6).** Discovery beyond trackers/DHT; without them, trackerless
   and LAN swarms are fragile and the DHT is the only fallback.

Honourable mentions: uTP (A13), BEP 47 padding (A14), web seeds (A11), v2 Merkle (A15).

## 5. Suggested implementation order

1. **Correctness hardening (≈1 week).** Poison attribution + ban after a corrupt piece; require
   in-flight membership for non-endgame blocks; per-block deadline with `cancel` on requeue;
   reserve inbound slots only after handshake and add per-IP/global half-open caps; fix tracker
   `left`/`completed` and upload accounting. Add regression tests for each (none of these paths
   has one today).
2. **Traffic control + reachability (≈2–3 weeks).** Wire `RateLimiter` into the peer writer with
   global and per-torrent caps, classify LAN via `TrafficClass::Local`, add PCP/NAT-PMP then UPnP,
   add a dual-stack listener and a bind/interface option (socket2). Exit: caps visible in config
   and enforced; inbound works without a manual forward.
3. **Streaming API (≈1–2 weeks).** Add `set_stream_position(file, offset)`, auto-advance the
   picker playhead from buffered ranges, expose contiguous buffered ranges and file completion
   in the API DTOs, gate Play on real buffered data, and allow `RarestFirst` for bulk mode.
4. **MSE/PE, then uTP (≈3–5 weeks).** MSE first (private-tracker blocker), then uTP/LEDBAT behind
   the same peer-session interface; add tracker crypto params with E1.
5. **Discovery completeness (≈2–3 weeks).** BEP 10 handshake in the swarm, `ut_metadata` serving,
   PEX, LSD, DHT IPv6 + `port` message + bucket refresh/age-out.
6. **Wire completeness (≈3–5 weeks).** v2 Merkle verification, BEP 47 padding files, web seeds,
   scrape, WebSocket trackers, fast extension, BEP 12 tiers.
7. **Long tail.** Holepunch (BEP 55), IP-filter formats, proxy support, interface binding polish.

Dependency notes: A7/A5 need A6; A16 needs A15; A12's encryption promise needs E1; A17 needs
D1/D3; C4/C3 need C1; B14 must precede private-tracker testing.

## 6. What I could not verify

- No live swarm, tracker, DHT soak or `cargo test` run; all claims are static code reading. The
  e2e/magnet tests cited are test definitions, not results I executed.
- Whether a given private tracker accepts `left=0` for partially selected torrents or requires
  MSE: tracker policy varies and no tracker list/config is in the repo.
- Real VPN/interface binding and IPv6 reachability on the target machine; there is no config or
  test harness for either.
- Whether `reqwest`'s environment-proxy behaviour is active in this build (no explicit proxy
  config); peer wire/DHT definitely bypass any proxy.
- Long-run DHT routing-table staleness (no age-out logic exists, but impact depends on churn).
- Actual mpv behaviour when handed a partial file; the app code path is clear, the runtime result
  is not.

## 7. Doc drift found while auditing

These are not functional gaps, but they misstate the requirement baseline:
`flux-daemon/src/lib.rs:5-7` says there are no trackers/DHT and "nothing can be downloaded";
`session.rs:91-98` says Active cannot receive and Seeding serves nobody; `config.rs:30` says the
port is "stored only"; `config.rs:103` says "TCP/uTP" though only TCP binds; `peer.rs:90-92` says
there is no DHT; `flux-net/src/lib.rs:9` says DHT is absent; `metadata.rs:10-22` says the peer
actor does not exist; `magnet.rs:251` says there is no DHT. `ui-spec.md:825-828` says Play is
disabled for incomplete files, but `app.rs:1171-1181` enables it for every media file.
