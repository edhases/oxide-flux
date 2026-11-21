# Oxide Flux — UI/UX Reference Log

Date of research: 2026-11-21. Companion to `ui-spec.md`.
All URLs were fetched on 2026-11-21 unless a publication date is given. “Accessed” means the
page carries no reliable publication date. Screenshot colors were sampled from JPEG/PNG
pixels and are approximate (±3 per channel).

---

## 1. Sources and what was taken

### 1.1 Free Download Manager (visual language)

| # | App / version | Date | URL | What was taken |
|---|---|---|---|---|
| 1 | FDM 6.29 announcement (new desktop UI) | 2025-07-29 | https://www.freedownloadmanager.org/blog?p=712 | Confirms the 2024+ redesign, its opt-in/opt-out switch, and that the redesign is a first-class “new UI style”, not a skin. |
| 2 | FDM 6.34 announcement | 2026-05-19 | https://www.freedownloadmanager.org/blog?p=720 | Confirms the redesign is still evolving and performance-focused; used to date the visual language. |
| 3 | FDM 6.36 screenshot (6.36.0) | 2026-11-11 | https://www.free-codecs.com/free-download-manager_screenshots.htm · image: https://www.free-codecs.com/pictures/screenshots/fdm.jpg | The primary visual reference: ~48 px top row, pill “+ ADD DOWNLOAD”, leading-icon search, 32 px table rows, 6 px progress bars, dashed drop strip, compact status bar with bordered speed pill and HIGH badge. Sampled accent `#4180E1`, light canvas `#ECF0F3`, surface `#FDFDFD`, track `#D1D5E0`. |
| 4 | FDM 6.36 changelog | 2026-11-10 | https://www.freedownloadmanager.org/changelog.htm | Version/date anchor; confirms UI iteration continues. |
| 5 | FDM forum: “FDM6 new design. Your feedback is welcome!” | thread 2024-12-27, last post 2026-10-25 | https://www.freedownloadmanager.org/board/viewtopic.php?t=19012 | Real user feedback on the redesign: dark-mode progress bars turning grey until completion, missing “done/100%” copy, invisible dialog edges, inconsistent preference font sizes, unreadable tooltips, always-visible scrollbar request, title bar color, and the light theme’s harsh bluish white. These became explicit acceptance rules in `ui-spec.md` §2.3, §4.6, §4.14, §4.16. |
| 6 | FDM guide / “Add download” flow | accessed | https://www.freedownloadmanager.org/landing.htm?from=b_dfp | Confirms the single “Add download” entry point and paste-URL flow. We kept one primary Add action but expanded it into a sheet with file review. |
| 7 | FDM tutorial (third-party) | 2025-10-01, updated 2026-11-11 | https://tutorials.mediaket.net/software-tutorials/free-download-manager-tutorial.html | Confirms Preferences uses left-hand categories and Theme = Light/Dark/System. Used only for layout confirmation; not a visual source. |
| 7a | FDM dark-mode screenshot (forum post by hajj_3, “Completed text grey / remove 0 B/s”) | 2025-04-22 | https://www.freedownloadmanager.org/board/viewtopic.php?t=19012&start=15 · image: https://i.imgur.com/O2q1jds.png | The dark-mode reference: near-black title bar `#000000`, app surface `#141415`, row `#222325`, hairline `#383A3D`, search fill `#27282A`, CTA `#73CEFC` with a black label, status bar `#141415`. Confirms FDM uses a light accent fill + dark label in dark mode, and confirms the “0 B/s everywhere / completed looks disabled” problems we explicitly fix. |

**FDM observations used as rules:**
- One saturated blue for primary action/progress; neutral surfaces otherwise.
- Fully rounded CTAs, 8 px rounded search/inputs, generous row padding.
- Search is a filled field with a leading magnifier, not a bordered form input.
- The dashed drop zone remains visible below the list, not only when empty.
- Status bar is a thin strip with grouped, bordered telemetry, not a sentence.
- A row-level overflow menu (`⋮`) appears on hover; checkboxes and file-type icons aid scanning.
- Dark mode is near-black (`#141415`) with slightly lifted rows (`#222325`), `#383A3D`
  hairlines, a `#27282A` search fill, and a **light** CTA (`#73CEFC`) with a dark label. This
  is the pattern our dark theme adopts: bright blue fill + near-black label, deep blue fill +
  white label in light.

**FDM details intentionally not copied:** the exact `#4180E1` (white text is ≈4.0:1), the
custom title bar, the typography (Segoe UI), the icon set, the “FDM” wordmark/logo, the exact
header height, the mobile layout, the browser-integration flows, and the ad-adjacent visual
density. No FDM asset is shipped.

### 1.2 qBittorrent (information architecture)

| # | App / version | Date | URL | What was taken |
|---|---|---|---|---|
| 8 | qBittorrent v5.0.0 release/changelog | 2024-09-29 | https://www.qbittorrent.org/news.html · https://github.com/qbittorrent/qBittorrent/blob/master/Changelog | Confirms 5.x baseline: Qt 6.7, “Start/Stop” language, tracker-error filter, privateness in UI, larger feature set. |
| 9 | qBittorrent 5.0.1 screenshot (Windows 10) | 2024-11-14 | https://commons.wikimedia.org/wiki/File:QBittorrent_5.0.1_screenshot.png | The IA reference: STATUS/CATEGORIES/TAGS/TRACKERS sidebar with counts, icon toolbar (add/add-link/delete/start/stop/settings), search + “Filter by” + lock, 7 visible table columns, bottom details tabs General/Trackers/Peers/HTTP Sources/Content, status bar with DHT nodes and two rate cells. Rows are ~22 px — deliberately **not** copied. |
| 10 | qBittorrent v5.3.0 release | 2026-10-19 | https://www.qbittorrent.org/news.html | Current 5.x state: virtual lists by default in WebUI, “Contribution” column in Peers, filter by infohash, content “Copy path”, session pause/resume. Confirms the direction of travel (more columns, virtualized tables). |
| 11 | qBittorrent 5.3 coverage (Linuxiac) | 2026-10-20 | https://linuxiac.com/qbittorrent-5-3-released-with-web-ui-improvements-security-fixes | Cross-check for 5.3 table/selection features; confirms virtual lists are now expected at scale. |
| 12 | qBittorrent wiki: FAQ + hotkeys | accessed | https://github.com/qbittorrent/qBittorrent/wiki/Frequently-Asked-Questions | Shortcut conventions (`Ctrl+O`, `Ctrl+Shift+O`, `Ctrl+S/P`, `Ctrl+Shift+P/S`, `Alt+O`), Return = double-click action, private torrents bypass DHT/PEX/LSD. |
| 13 | qBittorrent wiki: Explanation of Options | edited 2026-09-03 | https://github.com/qbittorrent/qBittorrent/wiki/Explanation-of-Options-in-qBittorrent | Settings grouping and semantics: Behavior, Downloads, Connection, Speed, BitTorrent, Advanced; transfer-list refresh interval; double-click action; torrent content/priority options. |
| 14 | qBittorrent source: `transferlistmodel.h` | master, accessed | https://github.com/qbittorrent/qBittorrent/blob/master/src/gui/transferlistmodel.h | The full column taxonomy: queue position, name, size, total size, progress, status, seeds, peers, down/up speed, ETA, ratio, popularity, category, tags, dates, tracker, limits, amounts, remaining, elapsed, save path, availability, infohash v1/v2, reannounce, private, created. Used to define our default vs optional/gated columns. |
| 15 | qBittorrent source: `transferlistwidget.cpp` | master, accessed | https://github.com/qbittorrent/qBittorrent/blob/master/src/gui/transferlistwidget.cpp | Default hidden columns, context-menu actions, sequential-download toggle, column visibility menu, modifier selection, double-click actions. Confirms we should not clone the 30-column default. |
| 16 | qBittorrent 5 dark-theme issue thread | opened 2024-09-30, active 2026 | https://github.com/qbittorrent/qBittorrent/issues/21427 | Documents dark-theme failures: blue text on charcoal, low-contrast alternating rows, pressed tabs not distinct. Used to justify explicit state colors and a selected bar, not a background-only selection. |

**qBittorrent observations used as rules:**
- Status tree is the primary navigation; counts are part of the label, not a separate widget.
- Toolbar is action-first, icon-led, grouped by task (add/remove | start/stop | settings).
- Details are tabbed and belong to the selected torrent; the tab set is stable.
- The status bar is telemetry: DHT, limits, down/up, not a marketing sentence.
- Dense tables need a column chooser and persisted widths.

**qBittorrent details intentionally not copied:** the 22 px row height, the 30+ column default,
the Qt/Fusion look, the GPL code or icons, the exact menu structure, the “Force start /
super seeding / recheck” features we do not implement, its logo, and its content/tracker
wording. We copy patterns, not files or brand.

### 1.3 Streaming-capable clients (play-while-downloading affordances)

| # | App / version | Date | URL | What was taken |
|---|---|---|---|---|
| 17 | WebTorrent Desktop | accessed | https://webtorrent.io/desktop | Play is a first-class action on a torrent; playback starts before completion; seeking reprioritizes pieces. The player is the product, not a plugin. |
| 18 | WebTorrent FAQ | accessed | https://webtorrent.io/faq | Sequential/in-order streaming trade-off: useful for media, but can reduce swarm health vs rarest-first. Supports our explicit “Download in order” opt-in, not a silent global default. |
| 19 | WebTorrent Desktop review (BetaNews) | 2016-04-08 | https://betanews.com/article/stream-torrents-before-theyre-fully-downloaded-with-webtorrent-desktop | Concrete flow: hover a title → Play/Download/Close; progress indicator shows downloaded portion; seeking reprioritizes; pause to let the buffer catch up. Older but behaviorally relevant. |
| 20 | PikaTorrent 0.15.0 | 2026-01-11 | https://www.pikatorrent.com/ · https://flathub.org/en/apps/com.pikatorrent.PikaTorrent | “Playable files” filter in torrents with many files, built-in player with subtitles, file selection before/after start, dark mode. We adopt the Playable filter and subtitle affordance, not its mobile layout. |
| 21 | Motrix manual: interface + tasks | accessed (v2.x) | https://motrix.app/manual/interface-overview/ · https://motrix.app/manual/managing-tasks/ | Detail panel tabs (Overview/Files/Pieces/Peers/Trackers/Activity), bottom stats bar with engine/NAT badges, and especially the **unknown vs zero** formatting rule (real zero `0 B/s`, unknown `–`, not applicable blank). This rule is in `ui-spec.md` §7.1. |
| 22 | Transmission 4.1.0 release | 2026-01-27 | https://github.com/transmission/transmission/releases/tag/4.1.0 | Sequential download is opt-in and can start from a chosen piece; queue/bandwidth priority is separate from file priority. Confirms the toggle + starting-position model. |
| 23 | Transmission RPC spec (`sequential_download`, priorities) | accessed | https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md | Confirms `sequential_download`, `sequential_download_from_piece`, `priority_high/low/normal`, and queue position are distinct concepts. Our UI keeps “Download in order” per torrent and file priority per file. |
| 24 | Stremio Tech Update #41 | 2025-04-02 | https://blog.stremio.com/stremio-tech-update-41-stremio-v5-and-stremio-web-updated | Player seekbar visibility, context menu, pause-on-minimize, volume UX. Only player affordances taken; not the content/catalog model. |
| 25 | Stremio “Getting started” (Board/Library) | updated 2025-08-27 | https://blog.stremio.com/getting-started-with-stremio-gifs-inside | “Continue watching” as a resume surface with progress; library sorting by recency/watched. Used only for the P2 Continue-watching strip. |

**Streaming observations used as rules:**
- A visible, explicit Play control beats a hidden “open with” flow.
- The seek bar must distinguish played vs buffered; the buffer is a real measurement.
- File selection and “playable only” filtering are essential in multi-file torrents.
- Sequential download is a user choice with a trade-off, not an invisible default.
- “Continue watching” belongs in the library as a resume strip, not as a separate content
  business model.

**Intentionally not copied:** WebTorrent/PikaTorrent/Motrix/Transmission/Stremio branding,
icons, poster art, catalog metadata, streaming-source integrations, mobile navigation,
WebRTC/WebTorrent protocol assumptions, and any code. We do not implement “stream any codec”;
mpv handles codecs, and the UI must state when playback is unavailable.

### 1.4 Desktop design systems (numbers, states, scales)

| # | System / version | Date | URL | What was taken |
|---|---|---|---|---|
| 26 | IBM Carbon data table | updated 2026-09-30 | https://www.carbondesignsystem.com/building-blocks/core/components/data-table/guidelines | Row/header height ladder 24/32/40/48/64; header and row heights must match; toolbar height matches the row; 14 px body, 14/600 column header. We chose 32 default / 28 compact as a compromise between Carbon’s 32 and qBittorrent’s 22. |
| 27 | IBM Carbon spacing | updated 2026-09-30 | https://www.carbondesignsystem.com/building-blocks/foundations/spacing | 2/4/8/12/16/24/32/40/48 spacing scale; 8 px base rhythm. Our scale is the requested 4/8/12/16/24/32 plus 2 for optical work. |
| 28 | IBM Carbon typography | updated 2026-09-29 | https://www.carbondesignsystem.com/building-blocks/foundations/typography/type-sets | 12/16, 14/20, 16/24, 20/28 productive scale; body 14, compact 14. We shifted one step down (13/18 base) to fit a desktop torrent table without 22 px rows. |
| 29 | Material Design 3 — state layers | accessed | https://m3.material.io/foundations/interaction/states/state-layers | Hover 8%, focus 10%, pressed 10%, drag 16% opacity; disabled content 38%; state layer 40 dp vs 48 dp target. We translated these to explicit token pairs because iced cannot composite state layers inside stock widgets. |
| 30 | Fluent 2 — shapes | accessed | https://fluent2.microsoft.design/shapes | Default 4 px radius, 2 px under 32 px, 8/12 for large surfaces; pill for tags/selection. Our 4/8/12/pill scale follows this, with a larger pill CTA from FDM. |
| 31 | Fluent 2 — layout / spacing | accessed | https://fluent2.microsoft.design/layout | 4 px base ramp (2/4/6/8/10/12/16/20/24/28/32), 44×44 iOS / 48×48 Android targets, spacing used to express grouping. We use 32 px desktop targets and 24 px page gutters. |
| 32 | Radix Colors — scales | accessed | https://www.radix-ui.com/colors/docs/palette-composition/scales | The 12-step role model (app bg, subtle bg, UI element, border, hover, solid, text, high-contrast text). Our dark ramp follows the same step logic even though the hexes are custom. |
| 33 | WCAG 2.2 | W3C Recommendation 2023-10-05 | https://www.w3.org/TR/WCAG22/ | 4.5:1 text / 3:1 large and non-text, 2.5.8 target size 24×24, 2.4.13 focus appearance (2 px perimeter, 3:1), 2.3.3 motion-from-interaction. All token pairs in `ui-spec.md` §2 were checked against these. |
| 34 | Microsoft title bar guidance | accessed | https://learn.microsoft.com/en-us/windows/apps/design/basics/titlebar-design | Native title bar behavior, Snap Layouts, system menu, high contrast. Basis for recommending native decorations instead of a custom FDM-style bar. |

**Design-system conflict:** Carbon’s default row is 40 px (medium), qBittorrent’s is 22 px, and
FDM’s screenshot is ~32 px. We chose 32 px default / 28 compact because it preserves FDM’s
visual rhythm while fitting 9–13 rows at 1280×800. This is a deliberate middle, not a
misreading of any source.

### 1.5 Implementation and asset licenses

| # | Asset / docs | Date | URL | What was taken |
|---|---|---|---|---|
| 35 | iced 0.14.0 docs | docs build dated 2026-10-27 | https://docs.rs/iced/0.14.0/iced/ | Confirms `Theme::custom` / `custom_with_fn`, `Palette` with `warning`, `theme::palette::Extended`, `svg::Style { color }`, `Table`/`table::Column`, `pane_grid`, `window::Settings` fields, `text::LineHeight`, and the absence of an accessibility feature. |
| 35a | iced `scrollable` source (`on_scroll`) | 0.14.0 / 0.14.2, accessed | https://raw.githubusercontent.com/iced-rs/iced/0.14.0/widget/src/scrollable.rs · https://raw.githubusercontent.com/iced-rs/iced/0.14.2/widget/src/scrollable.rs | `on_scroll` exists in 0.14.0 as `Fn(Viewport) -> Message`; later patches use `Scroll`/`Action`. This makes the P1 windowed list possible; the spec corrects an earlier assumption that the offset was unavailable. |
| 35b | iced accessibility + keyboard issues | issue #489 opened 2020-08-23; #552 opened 2020-10-05; both open as of 2026-11 | https://github.com/iced-rs/iced/issues/489 · https://github.com/iced-rs/iced/issues/552 | iced 0.14 still has no AccessKit integration and no automatic Tab traversal. The spec therefore includes an explicit `FocusChain` and marks screen-reader support as a P2 upstream dependency. |
| 36 | Lucide license | 2026 | https://github.com/lucide-icons/lucide/blob/main/LICENSE | ISC license; derived Feather icons note. Recommended icon set. |
| 37 | Inter license | 2026 | https://github.com/rsms/inter/blob/master/LICENSE.txt | SIL OFL 1.1; embedding/redistribution allowed with notice. Recommended UI font. |
| 38 | Tabler Icons license | 2026 | https://github.com/tabler/tabler-icons/blob/main/LICENSE | MIT. Alternative icon set considered; not chosen (more icons, slightly different stroke rhythm). |
| 39 | Phosphor Icons license | 2026 | https://github.com/phosphor-icons/core/blob/main/LICENSE | MIT. Alternative considered for its weight variants; not chosen (multiple weights invite mixing). |
| 40 | Material Symbols license | 2026 | https://github.com/google/material-design-icons/blob/master/LICENSE | Apache-2.0. Alternative considered; not chosen (variable-font/ligature integration is heavier in iced than SVG). |

---

## 2. Why Lucide + Inter

**Lucide (ISC).** 24×24 grid, 2 px stroke, round caps/joins, ~1,500 icons, one visual weight.
It covers every icon in the spec (`plus`, `magnet`, `search`, `play`, `pause`, `trash-2`,
`folder-open`, `settings`, `chevron-down`, `more-horizontal`, `arrow-down`, `arrow-up`,
`clock`, `check`, `alert-triangle`, `info`, `lock`, `globe`, `link`, `file`, `film`, `music`,
`subtitles`, `hard-drive`, `download`, `upload`, `list-filter`, `columns-3`, `grip-vertical`,
`maximize`, `volume-2`, `rotate-cw`, `wifi-off`). ISC is permissive, OSI-approved, and requires
only that the copyright/permission notice accompany copies. We embed SVGs, not a font, and
tint via `svg::Style { color }`. 40 icons ≈ 24 KB.

**Inter (OFL 1.1).** Designed for UI at small sizes, tall x-height, unambiguous `1/l/I` and
`0/O` in tables, excellent Windows ClearType rendering at 12–13 px. OFL allows embedding and
redistribution; the reserved font name only matters if we modify the font (we do not). Ship
static Regular/Medium/SemiBold TTFs, not the variable font, because iced/cosmic-text weight
selection from variable fonts is unreliable.

**Fallback.** If Inter fails to load, iced falls back to the system sans (Segoe UI on
Windows). We never ship Segoe UI (proprietary). CJK/Arabic glyphs use cosmic-text system
fallback; if a target locale shows tofu, add a Noto Sans subset, not a second UI family.

**Alternatives considered.** Tabler (MIT) is excellent and larger, but its stroke rhythm and
icon metaphors are less consistent with the Lucide set we standardized on. Phosphor (MIT) has
multiple weights (regular/bold/fill) that tempt inconsistent mixing. Material Symbols
(Apache-2.0) is a variable icon font; integrating it in iced means shipping a font and dealing
with ligatures/variation settings, whereas SVG handles tinting and exact sizing directly.
IBM Plex Sans (OFL) is a fine UI face but wider than Inter at 12–13 px; it would force more
truncation in the table. Geist (OFL) is modern but less tested at 12 px on Windows.

---

## 3. What NOT to copy (explicit)

**Trademarks and brand assets**
- No FDM name, logo, wordmark, “FDM” icon, installer art, marketing copy, or exact blue.
- No qBittorrent name/logo/icons, Qt/Fusion widget art, or GPL source files. qBittorrent is
  GPL-2.0-or-later; we did not copy code, icons, or theme resources.
- No WebTorrent, PikaTorrent, Motrix, Transmission, or Stremio names/logos/posters/art.
- No Windows Segoe UI files; use it only as a runtime system fallback.

**Layout clones**
- Do not reproduce FDM’s exact header height/filters/overflow arrangement or its custom title
  bar; our header has a different hierarchy (brand + search + split Add) and native chrome.
- Do not reproduce qBittorrent’s 22 px Qt table, 30-column default, menu bar, or exact details
  pane proportions. We keep the IA (sidebar groups, tabs, status telemetry) and change the
  density/visual language.
- Do not clone Stremio’s catalog/board model, poster grid, or content sources; only the
  resume/play affordance is relevant.
- Do not copy WebTorrent’s hover-only Play overlay; our Play is visible in the row menu, Files
  tab, and details header, and is keyboard reachable.

**Behavior we deliberately reject**
- FDM’s grey progress bar until completion (users reported it as unreadable). Our active
  progress is always accent.
- qBittorrent’s background-only selection and dark-theme blue text on charcoal. We add a 2 px
  selection bar and explicit state tokens.
- qBittorrent’s double-click-to-stop default. Our double-click is non-destructive.
- A dead, greyed-out menu full of unimplemented features. We hide unsupported actions.
- A fake DHT node count, fake seeds, fake per-file progress, or a fake buffer bar.

**Assets we do ship**
- Lucide SVGs (ISC) with `assets/icons/LICENSE`.
- Inter TTFs (OFL 1.1) with `assets/fonts/OFL.txt`.
- iced (MIT) as a dependency; its license is included by Cargo.
- Our own app mark/icon, generated in-house, not derived from any reference.

---

## 4. Conflicts between sources and how they were resolved

| Conflict | Sources | Resolution |
|---|---|---|
| Dark vs light primary | Brief says “dark charcoal”; FDM’s current main screenshot is light; a 2025 dark-mode screenshot exists in the feedback thread | Dark is the default and the acceptance-test theme. FDM’s light screenshot supplies geometry; the dark screenshot supplies the near-black surfaces and bright-fill/dark-label CTA pattern. Our dark hexes are tuned for AA, not copied. |
| Row density | FDM ~32 px; qBittorrent ~22 px; Carbon 40 px default | 32 px default, 28 px compact. Do not go below 28; the information set is qBittorrent’s, the rhythm is FDM’s. |
| Status count | qBittorrent has 14 statuses; the engine has 7 (`Queued/Checking/Downloading/Seeding/Complete/Paused/Error`) | Show 7 + “Completed” grouping. Do not invent Stalled/Moving/Forced. Add them when the engine reports them. |
| Column set | qBittorrent exposes ~38 columns; the API exposes 11 summary fields | 8 default columns backed by real data; optional columns gated by a documented API matrix. Seeds/Ratio/Availability are absent, not zero. |
| Double-click | qBittorrent default toggles start/stop; FDM opens the file | Open details; Play when there is exactly one playable file. Space toggles pause. Non-destructive wins. |
| Button shape | FDM pill CTAs; qBittorrent square-ish Qt buttons | Pill primary/secondary CTAs and segmented controls; 8 px icon buttons/inputs. One shape language, not a mix per screen. |
| Title bar | FDM custom dark bar; Windows native guidance | Native chrome for P0; custom bar is P2 only if Snap Layouts/high contrast/resize are preserved. |
| Settings layout | FDM Preferences with left categories; qBittorrent Options dialog with left icon list | FDM-style left nav + grouped rows, qBittorrent-style section semantics. 900×640 modal, not a separate window. |
| Accent contrast | FDM light `#4180E1` with white ≈4.0:1; FDM dark `#73CEFC` with black; qBittorrent dark blue text complaints | Use `accent` for non-text. Dark primary = `#6BB6FF` with `#08111F` label (8.79:1), following FDM's dark pattern. Light primary = `#2563EB` with white (5.17:1). Both beat the light FDM button and avoid the qBittorrent dark-text failure. |
| Light canvas | FDM light is bluish `#ECF0F3`; users call it harsh | Neutral `#F5F6F8`, white surfaces. |
| Play while downloading | WebTorrent/PikaTorrent make it central; our engine/API does not expose sequential streaming yet | Design the full player/affordance, gate Play for incomplete files with a truthful tooltip, and make the engine work an explicit P1 dependency. |
| Table implementation | iced 0.14 has a stock `Table`; 10k rows need windowing | Stock table only for P0 with a 500-row guard; the P1 windowed list uses `scrollable::on_scroll`; the custom `VirtualTable` is the final path. |

---

## 5. Method notes

- Screenshot colors: sampled with `System.Drawing.Bitmap.GetPixel` at known coordinates on
  the 1000×676 FDM JPEG and the 916×595 qBittorrent PNG. JPEG compression shifts values;
  treat them as roles, not exact brand values.
- Contrast: computed with the WCAG relative-luminance formula (sRGB, 0.04045 threshold) in a
  script; every pair in `ui-spec.md` §2 was checked. Border tokens used for input boundaries
  are separated from decorative hairlines because WCAG 1.4.11 applies to the former.
- iced APIs: verified against docs.rs for iced/iced_core 0.14.0 (released 2026-10-27), not
  against memory. `Theme::custom_with_fn`, `Extended`, `svg::Style.color`, `pane_grid`,
  `window::Settings`, `text::LineHeight`, and the absence of an accessibility feature were
  confirmed there.
- Dates: publication dates are from the page metadata/blog byline. “Accessed” means the page
  had no reliable date; the research date is 2026-11-21.
