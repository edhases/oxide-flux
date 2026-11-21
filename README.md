# Oxide Flux

A streaming-first BitTorrent client written in Rust: a headless engine plus a
native desktop app.

> **Status: preview.** It really downloads and seeds over TCP, announces to
> HTTP/UDP trackers, uses an IPv4 DHT, fetches magnets via `ut_metadata`,
> persists sessions, exposes a JSON-RPC/WebSocket API, and has a native iced UI
> with `.torrent`/`magnet:` associations. Playback is embedded: mpv is hosted in
> a child window and fed by a loopback HTTP range server that waits for verified
> pieces, so a media file can start before the torrent finishes. Still missing:
> MSE/PE encryption, uTP/LEDBAT, PEX/LSD, web seeds and v2 Merkle verification.

## Features

- **Protocol:** strict bencode, BitTorrent v1/v2/hybrid metadata, SHA-1 piece
  verification, TCP peer wire with BEP 10 extension pass-through, endgame with
  cancel batching, 10 s/30 s choking, sequential and rarest-first picking.
- **Discovery:** HTTP(S) and UDP (BEP 15) trackers with backoff, mainline DHT
  (IPv4), `ut_metadata` magnets, manual peers.
- **Storage:** torrent-space file layout (pieces span files), coalesced writes
  through a bounded RAM cache, preallocation (none/sparse/zeroed), threaded
  piece verification, safe session snapshots.
- **Safety:** path traversal and symlink/junction checks happen before any file
  is opened or resized; private torrents never touch the DHT; unsolicited blocks
  are rejected and a corrupt piece has an attributable source.
- **Control:** JSON-RPC 2.0 over HTTP, WebSocket events, a built-in static WebUI
  for headless servers, and a native desktop client that runs the engine
  in-process.
- **Desktop:** native iced 0.14 shell, embedded Inter + Lucide assets, search,
  sorting, selective file priorities, per-torrent playhead/streaming mode,
  embedded mpv playback (external player as fallback), graceful shutdown,
  single instance, per-user file associations.
- **Streaming:** a loopback HTTP range server serves only hash-verified bytes and
  blocks until the requested pieces arrive; `torrent.setStreamingMode` and
  `torrent.setPlayhead` steer the picker for playback.

## Architecture

| Crate | Responsibility |
| --- | --- |
| `flux-core` | bencode, metainfo, magnet parsing, path sanitization, piece picker. No I/O. |
| `flux-disk` | `BlockBackend`, torrent-space layout, coalescing writer, verifier. |
| `flux-net` | peer wire actor, HTTP/UDP trackers, DHT, `ut_metadata`, rate limiter. |
| `flux-api` | JSON-RPC/WebSocket transport, `Engine` trait, embedded WebUI. |
| `flux-daemon` | session actor, swarm, tracker service, magnet fetch, persistence, CLI. |
| `flux-app` | native iced client, in-process supervisor, associations, playback. |

The desktop app talks to the engine in-process through a bounded command/event
channel; the headless daemon serves the same engine over the API.

## Quick start

Playback embedding needs **mpv** on `PATH` (or `OXIDE_FLUX_MPV` pointing at
`mpv.exe`); without it the Play button falls back to an external player.

```sh
cargo run -p flux-app --bin oxide-flux        # desktop client
cargo run -p flux-daemon --bin fluxd -- verify file.torrent --dir D:/downloads
cargo run -p flux-daemon --bin fluxd -- --config fluxd.toml   # API + WebUI
```

`fluxd` prints the control API and WebUI URLs at startup. A config file only
needs the fields that differ from the defaults:

```toml
download_dir = "D:/downloads"
api_bind = "127.0.0.1:6800"
api_token = "change-me"      # required for non-loopback binds
listen_port = 6881
```

The desktop client registers `.torrent` and `magnet:` for the current Windows
user on first launch. Control it with `--register`, `--unregister`, or
`--no-register`; a second launch hands its target to the running window.

## API surface

`POST /rpc` and `GET /ws` (bearer token when configured):

`session.info`, `session.stats`, `torrent.list`, `torrent.get`, `torrent.add`,
`torrent.pause`, `torrent.resume`, `torrent.remove`,
`torrent.setFilePriorities`, `torrent.setPlayhead`,
`torrent.setStreamingMode`.

WebSocket notifications: `torrent.updated`, `torrent.removed`,
`session.shutdown`.

The loopback API requires `application/json` and a same-origin `Origin`/`Host`,
which blocks browser CSRF and DNS rebinding; a non-loopback bind refuses to start
without a token.

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Design and gap-analysis documents live in [`docs/design`](docs/design) and
[`docs/research`](docs/research). `AGENTS.md` is the working contract for
automated contributors.

## License

**GNU GPL v3.0 or later** — see [`LICENSE`](LICENSE).

The license is GPL because playback is designed around **mpv / libmpv**, which is
GPLv2-or-later; distributing an app that embeds it must be GPL-compatible.
Third-party assets and their licenses are listed in
[`THIRD_PARTY.md`](THIRD_PARTY.md). If the project ever drops embedded mpv and
relies only on an external player, a permissive `MIT OR Apache-2.0` relicensing
would be possible with the copyright holders' consent.
