# Third-party notices

Oxide Flux itself is licensed under GPL-3.0-or-later (see `LICENSE`). The
following third-party components are used or planned:

| Component | License | Where | Notes |
| --- | --- | --- | --- |
| [Lucide](https://lucide.dev) icons | ISC | `crates/flux-app/assets/icons/` | Full license text in `assets/icons/LICENSE.txt`. |
| [Inter](https://rsms.me/inter/) typeface | SIL OFL 1.1 | `crates/flux-app/assets/fonts/Inter-Variable.ttf` | Full license text in `assets/fonts/OFL.txt`. |
| [mpv](https://mpv.io) | GPLv2-or-later (typical builds) | optional playback engine: system `mpv.exe`, `OXIDE_FLUX_MPV`, or the in-app **Settings → Install playback engine** download; hosted in a child window by `flux-app` | Not bundled in the repository, archives or installer. The in-app installer downloads a pinned [shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake) build (`20261111`, SHA-256 `f259b5fd…2baf`) into `%APPDATA%\OxideFlux\playback\mpv`; the user triggers it, and mpv's GPL/source obligations follow its upstream project. |
| [NSIS](https://nsis.sourceforge.io) (Nullsoft Scriptable Install System) | zlib/libpng | build-time only, `installer/oxide-flux.nsi` | Used on the CI Windows runner to compile `oxide-flux-<tag>-windows-x86_64-setup.exe`. NSIS code is not linked into or shipped with Oxide Flux. |
| Rust crates | permissive (MIT/Apache-2.0/BSD family) | `Cargo.lock` | `cargo deny check` runs in CI (licenses, advisories, bans, sources). |
| Windows API bindings (`windows-sys`, `winreg`) | MIT OR Apache-2.0 | `flux-app` (Windows only) | — |

No Free Download Manager, qBittorrent, or other client assets, icons, or
trademarks are included. The UI takes inspiration from their layouts; it does not
copy their artwork.
