# Oxide Flux — UI/UX & Playback Gap Audit

Scope: `crates/flux-app` (native iced 0.14 client) and the DTOs in `crates/flux-api`, against
`docs/design/ui-spec.md` (baseline, 2026-11-21). Evidence is `file:line` at the time of writing.
Method: read every app source file, `AGENTS.md`, the spec, `references.md`, the DTOs/config, and
verified iced APIs in `~/.cargo/registry` (`iced 0.14.0`, `iced_widget 0.14.2`,
`iced_winit 0.14.1`, `iced_core 0.14.0`, `raw-window-handle 0.6.2`) rather than from memory.

Verdict: the color layer landed; the **geometry layer, the interaction layer, and both modal
screens did not**. Of 35 registered gaps, 21 are P0 and several violate the spec's two override
rules (no unmeasurable numbers; one accent/one icon set) — see G08, G12, G16.

Priority = spec P0/P1/P2. Effort: S ≤ ½ day, M 1–3 days, L > 3 days. Totals at the end.

## 1. Implemented vs spec (P0 audit)

| ID | Requirement | Current state | Evidence | Exact deviation | Eff | P | Acceptance |
|---|---|---|---|---|---|---|---|
| G01 | Token layer applied everywhere | Partial | `theme.rs:61-98`; `style.rs` | Colors only. No `space`/`radius`/`type`/`metrics` struct; every padding/radius is a literal (`app.rs:524,671,816`), incl. a non-token radius `10.0` (`style.rs:177`). `info_soft`, scrollbar and scrim tokens missing (`theme.rs:13-50`). | M | P0 | `theme.rs` exposes `space[8]`, `radius[4]`, `metrics`; `rg "padding\(\[[0-9]" crates/flux-app/src` → 0 hits. |
| G02 | `custom_with_fn` theme; stock widgets share the ramp | Missing | `theme.rs:101-113` | Uses `Theme::custom` (generated `Extended`). Pick-list **menu**, tooltip, scrollbar and any future checkbox/toggler render iced-generated tints, not tokens. This is §9.3's "single most important integration detail". | S | P0 | `theme.rs` builds `Extended` from tokens; screenshot of an open priority menu shows `bg.raised #1B1F26`, not a generated gray. |
| G03 | Typography | Partial | `app.rs:41,46-64` | Ships `Inter-Variable.ttf`, not static 400/500/600 (§9.4 says variable weight selection is unreliable). No `LineHeight` anywhere, so row heights are font-dependent (§2.4 requires explicit line height). `size(10)` labels (`app.rs:1272`) fall below the 11 px floor. | S | P0 | Static TTFs embedded; `ui_font` maps one file per weight; grep `LineHeight::Absolute` present in every text style; no `size(10)`. |
| G04 | Native chrome + window settings | Partial | `app.rs:31-44` | Title/size/min-size correct. `icon: None`, `exit_on_close_request` left at default `true` (no save-on-close), no `Resized`/`Rescaled`/`CloseRequested` subscription, so nothing is persisted (`iced_core/.../settings.rs:121`). | S | P0 | 256² RGBA icon passed; `exit_on_close_request:false`; closing saves `ui-state.json`. |
| G05 | Header: brand 24 + 15/600 wordmark + 11 version, search 320×32, Add split 120×36, overflow 32 | Partial | `app.rs:495-559` | Brand glyph 18 (`:497`), wordmark 17 (`:508`), no overflow menu, Add is two flat buttons (no split, no chevron), search is 300 wide with **no leading icon and no clear button** (`:520-524`), and no overflow menu. | M | P0 | 1280×800 screenshot measures 56 / 320×32 / 120×36 / 32×32; search shows `search` icon 16 at 10 px inset and an `x` when non-empty. |
| G06 | Toolbar: Pause/Resume/Remove; sort menu + Columns; disabled without selection | Partial | `app.rs:584-620` | No sort menu, no `Columns` control (so no column chooser either). Actions stay enabled with no selection and silently no-op (`:347-364`). "N of M" lives here (`:605-611`) instead of the content header (§4.8). | S | P0 | Buttons disabled with `—`-style tooltip when `selected.is_none()`; toolbar right side has sort + Columns; count moved to content header. |
| G07 | Sidebar groups + counts + Save-to; 34 px rows; 2 px selection bar | Partial | `app.rs:629-699`; `style.rs:93-114` | 7 groups ✓, counts ✓, zero counts shown ✓. `Completed` matches only `Complete`, excluding `Seeding` (§5.1 says it includes both) (`:109`). Row height ≈30 px (13/16 + 6+6 + 1 px border), not 34. Selected border width is `0.0`, so **there is no 2 px accent bar** (`style.rs:100`). No 64 px rail at <1080 (§3.3). | M | P0 | 34 px rows; selected item paints a 2 px `#3B82F6` left strip; unit test: Seeding torrent counts in both Seeding and Completed; §3.3 rail at <1080. |
| G08 | Table header: 32 px, 12/500, bottom 1 px `border.strong`, sort cycle | Partial | `app.rs:784-842` | Height 32 ✓ and 3-state sort ✓, but labels are 11 px (`:799`); the container border is `width: 0.0` (`:832-840`) so there is **no bottom rule at all**; column order is Name/Size/Progress/**Down/Up/Peers/Status**/ETA (`:818-825`) vs §6's Status/Peers/Down/Up; no resize handles, no focus. | S | P0 | Header renders a 1 px `#3C4450` bottom border; labels 12/500; column order matches §6; click cycles asc→desc→none in a unit test. |
| G09 | Table row: 32 px, separator, ellipsis, right-aligned numbers, row menu, dbl-click | Partial | `app.rs:844-916` | No `border.subtle` separator; **no 2 px selection bar** (`style.rs:123`); names use default `Wrapping::Word` and iced 0.14 `text` has no ellipsis, so long names wrap/clip inside 32 px; numeric cells are fixed-width but **left-aligned** (no `align_x`); no hover `more-horizontal`; no double-click (`Message` has only `Select`). | M | P0 | Row draws 1 px `#262C35`; name is truncated with `…` and full text in a tooltip; numbers right-aligned; hover reveals 28×28 menu; double-click opens details. |
| G10 | Progress: state-aware fills | Partial | `style.rs:245-256`; `app.rs:863` | One static style: paused stays `accent`, error stays `accent`, checking stays `accent`. §4.6 requires paused→`text.muted`, error→`danger`, checking→`warning` (and a capped bar when unknown). 100 % label is `success` ✓. | S | P0 | Four `progress_bar` styles keyed on `TorrentState`; unit test asserts fill for `Paused == #858F9D`. |
| G11 | Details pane: `pane_grid`, collapse to 40, resize 160–400, header identity | Partial | `app.rs:1042-1092` | Fixed `height(232)` container; no splitter, no collapse, not resizable. Header lacks the state badge and collapse chevron and has no Play. Progress bar is 220 (max 360) and the row has no fixed 40 px height. | M | P0 | `pane_grid` with two panes, drag 160–400, collapse to a 40 px tab strip, state badge in the identity row. |
| G12 | General tab: Size, Progress, Pieces, Piece length, Save path, State, Peers, Down, Up, ETA, Private | Partial / wrong | `app.rs:1120-1146` | Missing Progress, State, Piece length, Up, ETA. Row labelled **"Downloaded" prints a rate** (`:1130-1133`) — a number labelled as a total. Info hash printed full, proportional font, no grouping/copy (§7.8). Labels 11 px, values 11 px (spec 12/13). | S | P0 | All §5.4 rows present; `Downloaded` removed; hash in 12 px mono, 8-char groups, copy button. |
| G13 | Files tab: tree, checkbox, All/None/Playable, 3 priorities, Play gated | Partial / dishonest | `app.rs:1148-1186`; `player.rs:125` | Flat list, no checkbox, no folder tree, no All/None/Playable filter, no 28 px rows. **Play is enabled for any media file regardless of progress** (`:1171-1181`) while §5.4 says it must be disabled until 100 %/streaming exists. `Priority::Low` is silently displayed as `normal` (`:181-189`). | M | P0 | Play disabled with tooltip "Streaming playback needs sequential download support" unless `progress >= 1.0`; `Low` shown read-only; tree/filters per §5.4. |
| G14 | Status bar: 28 px counts, DHT on/off, ↓/↑, session ratio, selection | Partial | `app.rs:1209-1248` | 28 px ✓, counts/DHT/rates ✓. Ratio prints `0.00` when nothing was downloaded (`:1210-1214`) — spec says `—`; label is `ratio`, not `session`; no selection text; DHT text has no explanatory tooltip. | S | P0 | Ratio renders `—` at zero; label "session"; "N selected" appears; DHT tooltip states node count is unreported. |
| G15 | Empty state + dashed drop zone | Partial | `app.rs:939-1040` | Hero/title/body/actions ✓. Drop zone is 170 px (spec 180) and its border is **solid**, because `iced::Border` cannot draw dashes (`:986-990`); no full-window drag overlay when the list is non-empty; drag state only recolors the empty zone. | M | P0 | 180 px zone; 2 px dashed accent on drag-over (custom draw); non-empty list shows the full-content drop overlay. |
| G16 | Inline error banner above the table + toast queue | Partial / misplaced | `app.rs:477-493,1188-1207` | Banner is appended **after the status bar**, so it sits at the very bottom of the window and pushes the status bar up; no 3 px `danger` left border; only "Dismiss", no Retry/Reconnect; engine-stopped copy is `"engine stopped"` instead of §4.13's copy; no toasts at all. | S | P0 | Banner renders between toolbar and table, refuses to move the status bar; engine-stopped shows "Reconnect"; toast queue with 5 s auto-dismiss. |
| G17 | Add sheet (file/magnet, metadata, file tree, save path, start toggle) | Missing | `app.rs:200-215,331-346,561-582` | Adding is immediate: native picker → `AddBytes` with no review. Magnet opens an inline row that is pushed **below the status bar** (`:486-491`), not a modal. No duplicate "Open existing", no Start toggle, no save path, no file selection. | L | P0 | Modal per §5.3 with segmented source, parse/fetch states, file list, footer count; Esc closes; duplicate shows inline "Open existing". |
| G18 | Search: leading icon, clear, 150 ms debounce, Ctrl+F, result count, no-results | Partial | `app.rs:520-524,754-782` | Plain `text_input`, no icon/clear, filters on every keystroke, no `Ctrl+F`, no debounce; result count is in the toolbar; no-results state exists and has "Clear filters" ✓. | S | P0 | `search` icon inside the field; `x` clears; 150 ms debounce observable in a test; `Ctrl+F` focuses the field. |
| G19 | Keyboard: shortcuts, Tab order, 2 px focus rings | Missing | `app.rs:300-305` | The only subscriptions are a 100 ms timer and drag events. No keyboard listener, no `FocusChain`, no `focus_ring`, no `Esc`/`Space`/`Delete`. `operation::focus_next` exists in iced (`iced_core/.../focusable.rs:165`), so this is buildable now. | M | P0 | Tab order follows §7.7; focused control paints a 2 px `#60A5FA` ring; Space/Delete/Esc work. |
| G20 | Selection model: Ctrl/Shift, anchor, Ctrl+A, multi-select details | Missing | `app.rs:218-241,312-316` | `selected: Option<String>` only; no modifiers, no anchor, no keyboard movement, no aggregate header. Multi-select is P0.10. | M | P0 | Ctrl+click toggles, Shift+click ranges, Ctrl+A selects visible; details header shows "3 torrents selected". |
| G21 | Performance guard: 4 Hz snapshot, detail 1 Hz, no per-frame 10k work, 500-row cap | Missing | `app.rs:302,754-782,844-916`; `supervisor.rs:78` | Snapshot is 1 Hz ✓ and detail fetch is per snapshot ✓ (not 10 Hz). But `view()` builds a widget tree for **every** torrent, `visible()` allocates + lowercases + sorts on every view, sidebar counts re-filter 7×, and each row clones its `String` name. No 500-row guard, no `on_scroll` windowing. | L | P0 | With 10 000 synthetic summaries, `view()` builds ≤ `visible_rows+20` rows; second view with unchanged snapshot performs zero name allocations; frame time < 16 ms. |
| G22 | Drag/drop: batch, overlay, rejection toast | Partial | `app.rs:390-411,1257-1269` | One add per `FileDropped` (iced emits per file), no 100 ms collection, hover flag only affects the empty state, no batch toast, non-`.torrent` files produce an error banner (acceptable rejection copy). | M | P1 | Dropping 3 files produces one snapshot delta and one toast "3 added · N duplicates"; overlay counts files during hover. |

## 2. Missing components and screens

| ID | Screen / component | Evidence of absence | What it needs | Eff | P |
|---|---|---|---|---|---|
| M01 | Settings dialog (General/Downloads/Connection/Appearance/Advanced/About) | No `settings` module; `icons::SETTINGS` is embedded but unused (`icons.rs:29`) | 900×640 modal, left nav, 56 px rows, restart badges, dirty guard; only live settings (theme/density/motion/hide-zero) enabled | L | P1 |
| M02 | Torrent context menu | No `mouse_area`/`float`/`stack` usage anywhere | Positioned overlay per §4.15, only the 8 supported items, keyboard open via `Shift+F10` | M | P1 |
| M03 | Toast queue | `Message` has no toast variant; errors replace the single `error: Option<String>` | `stack!` + timed subscription, max 3, error persists | S | P1 |
| M04 | Tooltips | No `.tooltip(` call in `app.rs` | Stock `Tooltip` with `.delay(Duration::from_millis(500))` — verified available (`iced_widget/src/tooltip.rs:126`) | S | P1 |
| M05 | Column chooser + persisted widths | No column state; widths are literals | 240 px menu with checkboxes + "Reset columns"; `ui-state.json` | M | P1 |
| M06 | Density switch (28 px compact) | All heights hard-coded | `Metrics` with 32/28 row, 32/28 header, 44/40 toolbar | S | P1 |
| M07 | Light theme + `System` preference | `theme.rs` has one palette and no `Tokens` variant | Second token set (§2.2), `ThemePreference::{System,Dark,Light}`, OS detection | M | P1 |
| M08 | `ui-state.json` (window, nav, details, sort, filter, theme, density) | Only `launch.rs:50-90` stores `register_associations` | Versioned file, 500 ms debounce, corrupt→defaults+toast | M | P1 |
| M09 | Player IPC controls / track selection | `player.rs:65-81` spawns mpv with no `--input-ipc-server`; `title()` is never read | Named-pipe client, play/pause/seek/volume, audio+subtitle menus (§4 below) | L | P1 |
| M10 | Correct play path (per-torrent save dir) | `Message::Play` joins `self.download_dir` (`app.rs:384-389`) although `TorrentDetail.download_dir` exists (`engine.rs:138`) and `Event::Ready.download_dir` is discarded (`app.rs:424-427`) | Build the path from the selected detail, or store a per-id dir map | S | P1 |
| M11 | Duplicate handling ("Open existing") | Duplicate arrives as an `EngineError` string in the banner | Match `EngineError::Duplicate(id)` and select that row | S | P1 |
| M12 | i18n | All strings are literals in `app.rs` | Message catalog + locale switch; defer until the shell freezes | L | P2 |
| M13 | Tray, close-to-tray, completion notification | `platform.rs` only registers file associations | Tray crate, close behaviour setting, real state-diff notification | L | P2 |

## 3. Data gaps that block UI

| Data the UI needs | Blocking fact (DTO evidence) | What unblocks it |
|---|---|---|
| Add options: save path, paused, file selection, sequential | `AddTorrent` is only `Metainfo{bytes}` / `Magnet{uri}` (`engine.rs:171-184`); `torrent.add` takes that enum alone (`methods.rs:19-35`) | Extend `torrent.add` with `{dir, paused, files, priorities, sequential}` or accept add-then-`setFilePriorities` with a visible race |
| Per-file downloaded bytes / progress | `FileView { index, path, length, priority }` (`engine.rs:114-125`) | Add `downloaded: u64` or `have_bytes`; Files tab then gets the column §5.4 reserves |
| Peer list | `TorrentDetail` has no peers field (`engine.rs:127-139`) | `Vec<PeerView>{addr, client, progress, down, up, flags}` + `torrent.getPeers` |
| Tracker list | Same | `Vec<TrackerView>{url, status, seeds, peers, next_announce, message}` |
| Seeds/leeches, availability, per-torrent ratio | `TorrentSummary` has a single connected `peers: u32` (`engine.rs:106`) | Scrape/peer-manager data; never derive seeds from peers |
| Added/completed dates, category, tags, tracker, per-row save path | `TorrentSummary` (`engine.rs:86-111`) | Engine-side metadata; until then keep these columns hidden |
| Settings values in the UI | `Config` has the fields (`config.rs:96-124`) but no `session.config`/`session.setConfig` RPC; the app builds `Config::default()` itself (`app.rs:1358-1373`) | Config RPC, or a UI-owned config file that rebuilds the supervisor |
| Bandwidth / per-torrent limits | No rate fields exist in `Config` at all (`config.rs:96-124`) | Engine throttle first; do not ship the controls |
| Per-torrent error reason | `TorrentState::Error` carries no message (`engine.rs:49-65`) | `error: Option<String>` on the summary/detail |
| Complete/checking/error counts for the status bar | `Stats` has torrents/active/downloading/seeding/paused only (`engine.rs:141-161`) | Add counters or keep deriving from the list |

The embedded WebUI (`flux-api/static/`) is a development harness (token box, add form, flat list) and
intentionally does not mirror the spec; it only needs the same DTOs, so every gap above hits both UIs.

## 4. Playback: external mpv → embedded child window

Today: `Player::play` spawns `mpv --force-window=yes --keep-open=no <file>` with stdio nulled
(`player.rs:65-81`). Consequences: a separate top-level window, no controls, no error surface, and the
user's `mpv.conf` can force fullscreen/ontop because `--config=no` is not passed.

**Verified iced 0.14 facts (not guesses).**
- `iced::window::run(id, FnOnce(&dyn Window) -> T) -> Task<T>` exists; `Window: HasWindowHandle + HasDisplayHandle`, and `raw_window_handle` 0.6.2 is re-exported as `iced::window::raw_window_handle` (`iced_runtime-0.14.0/src/window.rs`, `Action::Run`).
- HWND: `RawWindowHandle::Win32(Win32WindowHandle { hwnd: NonZeroIsize, .. })` (`raw-window-handle-0.6.2/src/windows.rs:56`). `window::raw_id(id) -> Task<u64>` returns `window.raw.id().into()` (`iced_winit-0.14.1/src/lib.rs:1628`), a cheap alternative; `run` is the documented path.
- Get the id once with `window::oldest() -> Task<Option<Id>>`, or subscribe to `window::open_events()`.
- `window::Settings.platform_specific` on Windows has **only** `drag_and_drop`, `skip_taskbar`, `undecorated_shadow`, `corner_preference` (`iced_core/.../settings/windows.rs`) — no parent handle. A second iced window cannot be a child; `SetParent` on an iced surface is not supported.
- `window::Event::Resized(Size)` (logical) and `Rescaled(f32)` exist (`iced_core/.../event.rs:29-32`); `window::scale_factor(id)` is a `Task<f32>`.
- `iced_core::widget::operation` exposes only `focusable`, `scrollable`, `text_input` — **no bounds query**. iced cannot tell us where the player pane is; keep the rect in app metrics or add a bounds-reporting widget, which needs the `advanced` feature that `crates/flux-app/Cargo.toml` does not enable.
- No AccessKit anywhere in `iced_winit 0.14.1`.

**Windows embedding checklist.**
1. After the first frame, resolve the main HWND via `window::oldest()` + `window::run`.
2. Create one host child with `CreateWindowExW` (`WS_CHILD|WS_VISIBLE|WS_CLIPCHILDREN`, class `STATIC`, parent = iced HWND, no `WS_TABSTOP`) using the `windows` crate (`Win32_Foundation`, `Win32_UI_WindowsAndMessaging`). Keep it in `App`, not in `view()`.
3. Spawn mpv with `--wid=<host hwnd> --no-config --load-scripts=no --no-osc --no-border --no-input-default-bindings --input-vo-keyboard=no --keep-open=no --force-window=no --idle=yes --no-hidpi-window-scale --input-ipc-server=\\.\pipe\oxideflux-mpv-<pid>`. mpv creates its own `WS_CHILD` inside the host; do not assume the host resizes it.
4. On `Resized`/`Rescaled`, compute physical px (`logical × scale`) and `SetWindowPos` both the host and mpv's VO child (`EnumChildWindows`/`FindWindowEx`). Verify on the pinned mpv build whether the VO window equals the passed HWND.
5. Focus: clicking the child moves keyboard focus away from iced. Either accept mpv's bindings or subclass the child, `SetFocus(parent)` on `WM_LBUTTONDOWN`, and forward keys with IPC `keypress`. Without one of these, Space/arrows die while the video has focus.
6. Teardown order: kill mpv **then** destroy the child, or a black rectangle remains (the spec's P2 acceptance).
7. Non-Windows / failure: keep the external window fallback; never render an empty video pane as if playing.

**Controls, tracks, buffering.** IPC is a JSON named pipe: `{"command":["set_property","pause",true]}`; `observe_property` for `time-pos`, `duration`, `pause`, `eof-reached`, `paused-for-cache`, `cache-buffering-state`, `track-list`, `aid`, `sid`, `volume`. Track menus read `track-list` (`lang`, `title`, `codec`, `default`, `forced`), set `aid`/`sid`, add externals with `sub-add` and `--sub-auto=fuzzy`. All of these are real measurements, but a **partially written file hits EOF at the write frontier**, so a buffered bar is only honest for completed files until the engine exposes contiguous ranges. Do not draw the §5.6 buffered layer before that.

**Licensing and packaging.** mpv is GPL-2.0-or-later by default (LGPL only with `--enable-lgpl`). Spawning `mpv.exe` as a separate process is aggregation, not linking, but redistributing the binary still requires GPL compliance: ship the license text and a source offer for the exact build. Linking libmpv (or `mpv_render_context`) would relicense Oxide Flux; the repo has **no project LICENSE** (only `assets/icons/LICENSE.txt` ISC and `assets/fonts/OFL.txt`), so pick a license before bundling. Pin a build hash, keep it in `assets/mpv/` or `%LOCALAPPDATA%\OxideFlux\mpv\`, and keep the existing `OXIDE_FLUX_MPV` override (`player.rs:99-121`).

## 5. Accessibility and performance

| Concern | Finding | Fix |
|---|---|---|
| Non-text contrast (1.4.11) | Inputs use `border.default #2E343E` on `bg.overlay #232831` (`style.rs:217`) ≈1.2:1; the spec's `border.input #667385` (3.69:1) is unused | Switch input/search border to `border_input`; assert in a unit test |
| Focus | No rings; text input focus is a 1 px accent border, not 2 px `focus.ring` (`style.rs:215`) | `FocusRing` wrapper with a reserved 2 px border |
| Hit targets | Toolbar buttons ≈28 px (`padding([6,12])` + 13 px text), tabs ≈28 px, sidebar ≈30 px vs the spec's 32 | Raise toolbar/tabs to 32; keeps WCAG 2.5.8 ≥24 |
| Disabled contrast | `text.disabled #5A6470` on `bg.overlay #232831` ≈2.2:1, below the spec's own 2.5:1 floor (`style.rs:42,53`) | Use `overlay` at 40 % accent for disabled primary, per §2.3 |
| Screen readers | iced 0.14 has no accessibility feature and `iced_winit 0.14.1` has no AccessKit dependency (verified in the registry) | Keyboard + contrast now; document the limitation in About |
| Reduced motion | No animation exists, so the toggle would be a dead setting | Ship the toggle with the first transition, not before |
| Scaling / narrow windows | Fixed px columns; Name has no 220 px minimum, so at 960 px it clips instead of scrolling horizontally | Enforce `min_width` + horizontal scroll per §7.8 |
| 10k torrents | Full row tree per view (`app.rs:844-916`), per-row `String::clone` (`:851`), `visible()` allocates/lowercases/sorts every view (`:754-782`), 7 extra filter passes for counts (`:632-636`) | Cache the filtered/sorted index per snapshot; render `visible_rows + 20`; `Arc<str>` names |
| Redraw cadence | `Pump` at 10 Hz (`app.rs:302`) vs snapshots at 1 Hz (`supervisor.rs:78`) | Pump only drains; request redraw when the snapshot or detail actually changed |
| Detail RPC | Fetched on every snapshot (`app.rs:431-433`) even when unchanged — 1 Hz, spec-compliant, still wasteful | Fetch on selection change + when `pieces_have`/state changed |
| Long lists memory | `Vec<TorrentSummary>` replaced per snapshot; names duplicated into per-row widgets | `Arc<str>` + index vector (~40 KB) per §9.8 |

## 6. Visual polish backlog — what still looks "default iced"

| # | Symptom | Cause | Concrete fix |
|---|---|---|---|
| V1 | Window reads as three floating rounded cards with gutters, not one app | `style::surface` radius `10.0` + border (`style.rs:174-180`), `body()` `spacing(10)` (`app.rs:622-627`) | Radius 0, no outline, 1 px `#262C35` separators; flush `row![nav, content]` |
| V2 | Selection is background-only (the exact qBittorrent dark-mode complaint) | border width `0.0` in `style::nav`/`style::row` (`style.rs:100,124`) | Leading 2 px `#3B82F6` strip child, label 600 |
| V3 | Drop target looks like a plain card | solid `Border`, radius 12 (`app.rs:986-990`) | Custom draw: 2 px dashed `#3C4450` → `#3B82F6` on hover; 180 px |
| V4 | Rows have no rhythm; long names collide | no separators, default word wrap (`app.rs:850-905`) | 1 px `#262C35` bottom rule, `Wrapping::None` + `…` truncation |
| V5 | Numbers look ragged | Left-aligned fixed-width text (`app.rs:855-900`) | `.align_x(Right)` on Size/Down/Up/Peers/ETA, `type.numeric` 12/500 |
| V6 | Header/brand looks homemade | 17 px wordmark in a 32 px accent tile (`app.rs:497-515`) | Mark 24 neutral `#E6E9EE`, wordmark 15/600, version 11 `#858F9D` |
| V7 | Details header is a plain row | no badge, 220 px bar inline (`app.rs:1045-1070`) | 40 px identity row: name 15/600, badge 22, bar 8 px max 360, chevron |
| V8 | Scrollbars are iced defaults | no `.style()` on `scrollable` (`app.rs:712,1084`) | 10 px rail transparent, 6 px thumb `#3C4450` / `#5A6470` |
| V9 | Status bar is a sentence | plain text runs (`app.rs:1217-1242`) | Bordered telemetry groups, `↓`/`↑` pills, ratio `—` |

## 7. Ordered backlog (top 10)

1. **Truth pass (S, P0):** delete "Sequential streaming ready" (`app.rs:926`) — it is false; render `—` for size/ETA before metadata, `—` for ratio at zero, state-colored progress, and disable Play unless complete.
2. **Theme + geometry tokens (M, P0):** `custom_with_fn` + `Extended`, then `space/radius/type/metrics`; remove every literal padding and the radius 10.
3. **Flush pane layout + selection bars (S/M, P0):** V1 + V2 — the single largest visual delta.
4. **Table shell correctness (M, P0):** separator, header rule, §6 column order, right-aligned numbers, ellipsis + tooltip, hover row menu, double-click.
5. **Keyboard + focus chain (M, P0):** key subscription, `focus_next/focus_previous`, rings, Esc/Ctrl+F/Space/Delete/Ctrl+A.
6. **Add sheet (L, P0):** modal, segmented source, parse/fetch states, file tree, priorities, Start toggle; save-path stays hidden until `torrent.add` accepts a directory.
7. **Details pane rework (M, P0):** `pane_grid`, collapse 40 / resize 160–400, state badge, header Play, corrected General rows.
8. **Errors: banner placement + toasts + duplicate "Open existing" (S/M, P1).**
9. **Settings (appearance only) + `ui-state.json` + window icon (L, P1):** theme/density/reduced motion/hide-zero are honest today; show connection/downloads rows read-only with a "restart" badge only after the config RPC exists.
10. **Table windowing (L, P1):** 500-row guard now, `scrollable::on_scroll` window next, `VirtualTable` last; cache the sorted/filtered index per snapshot.

## 8. Do not build yet (needs engine/API work first)

1. Seeds, ratio, availability, category, tags, added/completed, tracker, info-hash columns — no DTO fields.
2. Peers and Trackers tabs — `TorrentDetail` has neither list.
3. Play-while-downloading, buffered range, "streaming" copy — needs sequential mode, contiguous ranges, `set_stream_position`; until then Play stays completion-gated.
4. Add sheet's save path + per-file priorities **before** add — `torrent.add` cannot carry them (add-then-set is a race, not a feature).
5. Global/per-torrent bandwidth settings — no `Config` rate fields, no throttle.
6. Recheck, force-start, reannounce, super-seeding, queue priority — no engine ops; do not add greyed menu items.
7. Tracker scrape counts — no scrape RPC.
8. Continue-watching strip — no playback-position store.
9. Free-space line — UI-side stat is allowed, but only once the save path is per-torrent and real.

## 9. Totals

P0: 21 (G01–G21). P1: 12 (G22 + M01–M11). P2: 2 (M12, M13). M02/M05/M06/M07/M08/M09 are independent of engine work and can ship in parallel.

Biggest visual discrepancy vs the reference: the layout is **three 10 px-rounded, outlined cards separated by 10 px gutters** (`style.rs:174-180`, `app.rs:622-627`) instead of the spec's flush, full-bleed pane stack — so the window never reads as FDM/qBittorrent's single continuous surface, and the missing 2 px selection bar (V2) makes selection look like a hover.
