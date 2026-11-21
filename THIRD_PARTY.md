# Third-party notices

Oxide Flux itself is licensed under GPL-3.0-or-later (see `LICENSE`). The
following third-party components are used or planned:

| Component | License | Where | Notes |
| --- | --- | --- | --- |
| [Lucide](https://lucide.dev) icons | ISC | `crates/flux-app/assets/icons/` | Full license text in `assets/icons/LICENSE.txt`. |
| [Inter](https://rsms.me/inter/) typeface | SIL OFL 1.1 | `crates/flux-app/assets/fonts/Inter-Variable.ttf` | Full license text in `assets/fonts/OFL.txt`. |
| [mpv](https://mpv.io) | GPLv2-or-later (typical builds) | external process today; embedded child window planned | Not bundled in this repository. Distributing binaries with mpv requires GPL compliance and mpv's source offer. |
| Rust crates | permissive (MIT/Apache-2.0/BSD family) | `Cargo.lock` | Run `cargo deny check licenses` (planned CI gate) before releases. |
| Windows API bindings (`windows-sys`, `winreg`) | MIT OR Apache-2.0 | `flux-app` (Windows only) | — |

No Free Download Manager, qBittorrent, or other client assets, icons, or
trademarks are included. The UI takes inspiration from their layouts; it does not
copy their artwork.
