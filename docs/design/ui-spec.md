# Oxide Flux — UI/UX Design Specification

Status: design baseline for the native `iced 0.14` desktop client (`crates/flux-app`).
Date: 2026-11-21. Owner: UI/UX research subagent.
Scope: visual language, layout, components, interaction, accessibility, and an implementation
plan. Not application code.

Reference synthesis: **Free Download Manager 6.x (2024–2026 redesign)** supplies the visual
language (charcoal surfaces, one blue accent, pill CTAs, dashed drop zone, leading-icon search,
left-nav settings). **qBittorrent 5.x** supplies the information architecture (status/category/
tag/tracker tree, dense sortable table, tabbed details, status bar). Neither is cloned; see
`references.md` for sources, licenses, and the do-not-copy list.

Two rules override any aesthetic preference in this document:

1. **Never render a number the engine cannot measure.** No invented speeds, peer counts,
   seeds, ratios, availability, DHT node counts, or per-file progress. When a field is not
   reported, render `—` with a tooltip, or hide the control. This is the UI expression of
   `AGENTS.md`.
2. **One accent, one icon family, one type family.** Every colored pixel is traceable to a
   token in §2; every icon is Lucide; every glyph is Inter.

---

## 1. Design principles

1. **The transfer is the interface.** The table, progress, and rates are the product; chrome
   stays quiet and never competes with a moving progress bar.
2. **Dense, not cramped.** qBittorrent's information set at FDM's breathing room: 32 px rows,
   12 px cell padding, 24 px page gutters, one primary action per screen.
3. **Honest states beat optimistic ones.** Unknown is `—`, zero is `0 B/s`, blocked is a
   disabled control with a reason. Loading, empty, and error are designed screens, not blanks.
4. **Play is a first-class verb.** Any playable file exposes Play; playback is allowed before
   completion only when the engine can actually feed contiguous pieces.
5. **Keyboard is not a fallback.** Every action reachable by mouse is reachable by keyboard,
   with a visible 2 px focus ring and a documented shortcut.

---

## 2. Design tokens

All values are logical pixels at 100% scaling. iced multiplies by the window scale factor.
Hex values are sRGB. Contrast ratios are WCAG 2.x relative-luminance ratios, computed for the
stated pair.

### 2.1 Color roles — dark (default)

The dark ramp is a neutral charcoal with a faint blue cast, informed by FDM's dark build
(near-black `#141415` surfaces, `#222325` rows, `#383A3D` hairlines, bright cyan-blue CTA
`#73CEFC` with a dark label). We lift the surfaces slightly and use a less cyan blue so the
table stays readable for long sessions. The exact hexes below are Oxide Flux's; sampled FDM
values and their source are in `references.md`.

| Token | Hex | Use | Contrast check |
|---|---|---|---|
| `bg.canvas` | `#0F1115` | Window base, area behind panes | — |
| `bg.surface` | `#15181D` | Header, sidebar, table, details, status bar | text 14.62:1 |
| `bg.raised` | `#1B1F26` | Menus, modals, toasts, input fill | text 13.58:1 |
| `bg.overlay` | `#232831` | Pressed rows, segmented track, skeletons | text 12.0:1 |
| `border.subtle` | `#262C35` | Row separators, section rules (decorative) | — |
| `border.default` | `#2E343E` | Card and table outlines (decorative) | — |
| `border.strong` | `#3C4450` | Hover outline, scrollbar thumb | 1.81:1 vs surface |
| `border.input` | `#667385` | Input/checkbox boundary (required 3:1) | 3.69:1 vs surface, 3.43:1 vs raised |
| `text.primary` | `#E6E9EE` | Names, values, headings | 14.62:1 |
| `text.secondary` | `#A7B0BC` | Labels, metadata, inactive tabs | 8.12:1 |
| `text.muted` | `#858F9D` | Placeholders, `—`, timestamps, helper copy | 5.43:1 surface, 5.05:1 raised |
| `text.disabled` | `#5A6470` | Disabled labels (exempt, kept legible) | ~2.9:1 |
| `accent` | `#3B82F6` | Progress fill, icons, selection bar, focus-adjacent marks | 4.84:1 vs surface |
| `accent.hover` | `#5B9AF8` | Icon hover, outline hover | 6.29:1 |
| `accent.pressed` | `#2F6FD0` | Icon pressed | 3.9:1 |
| `accent.fill` | `#6BB6FF` | Primary button fill (dark label) | `accent.on` 8.79:1; vs surface 8.27:1 |
| `accent.fill.hover` | `#8CC8FF` | Primary hover (lightens) | `accent.on` 10.65:1 |
| `accent.fill.pressed` | `#4C9AFF` | Primary pressed | `accent.on` 6.64:1 |
| `accent.on` | `#08111F` | Label/icon on `accent.fill` in dark | 8.79:1 on `#6BB6FF` |
| `accent.text` | `#8AB8FF` | Text links and accent labels on raised | 8.80:1 vs surface |
| `accent.soft` | `#1B2A44` | Selected nav background, badge tints | — |
| `selection.bg` | `#20344F` | Selected table/detail row | text 10.36:1 |
| `selection.bar` | `#3B82F6` | 2 px left bar on selection | 3.43:1 vs selection |
| `focus.ring` | `#60A5FA` | 2 px keyboard focus border | 7.0:1 vs surface |
| `success` | `#34D399` | Seeding, completed, positive ratio | 9.26:1 |
| `success.soft` | `#12352A` | Success badge/banner fill | — |
| `warning` | `#FBBF24` | Checking, paused-by-limit, caution | 10.66:1 |
| `warning.soft` | `#3A2E12` | Warning badge/banner fill | — |
| `danger` | `#F87171` | Error, destructive text/icon | 6.43:1 |
| `danger.fill` | `#F87171` | Filled destructive (dark label) | `danger.on` 6.84:1 |
| `danger.fill.hover` | `#FCA5A5` | Filled destructive hover | `danger.on` 9.97:1 |
| `danger.fill.pressed` | `#EF4444` | Filled destructive pressed | `danger.on` 5.11:1 |
| `danger.on` | `#1A0A0A` | Label/icon on `danger.fill` in dark | 6.84:1 |
| `danger.soft` | `#3A1D1F` | Error badge/banner fill | — |
| `info` | `#8AB8FF` | Queued/metadata info | 8.8:1 |
| `info.soft` | `#1B2A44` | Info banner fill | — |
| `progress.track` | `#2A3039` | Progress track | — |
| `scrollbar.thumb` | `#3C4450` | Idle thumb | 1.81:1 (decorative) |
| `scrollbar.thumb.hover` | `#5A6470` | Hover thumb | 3.1:1 |
| `scrim` | `rgba(0,0,0,0.55)` | Modal backdrop | — |

**Why the primary fill changes with theme.** `accent` (`#3B82F6`) is the data hue: progress,
icons, selection. White text on it is 3.68:1 and fails AA at 13 px, so it is never a button
fill with a white label. In dark mode the primary button uses the lighter `accent.fill`
(`#6BB6FF`) with a near-black `accent.on` label — the same pattern as FDM's dark CTA
(`#73CEFC` with black text) — reaching 8.79:1 and making the one primary action unmistakable.
In light mode it uses `#2563EB` with white (5.17:1). This is also a deliberate correction of
the light FDM button (`#4180E1`, ≈4.0:1 with white) and of qBittorrent's dark-theme
blue-on-charcoal complaints.

### 2.2 Color roles — light variant

| Token | Hex | Use | Contrast check |
|---|---|---|---|
| `bg.canvas` | `#F5F6F8` | Window base | text 15.31:1 |
| `bg.surface` | `#FFFFFF` | Panels, table, header | text 16.56:1 |
| `bg.raised` | `#FFFFFF` | Menus, modals | — |
| `bg.overlay` | `#F0F2F5` | Hover rows, input fill, segmented track | text 14.77:1 |
| `border.subtle` | `#ECEEF2` | Row separators | — |
| `border.default` | `#DFE3E9` | Card outlines | — |
| `border.strong` | `#CBD2DB` | Hover outline | 1.52:1 |
| `border.input` | `#7C8798` | Input boundary (required 3:1) | 3.64:1 vs white |
| `text.primary` | `#1B1F24` | Names, values | 16.56:1 |
| `text.secondary` | `#5A6472` | Labels, metadata | 6.00:1 |
| `text.muted` | `#626D7D` | Placeholders, `—`, helper copy | 5.25:1 white, 4.85:1 canvas, 4.68:1 overlay |
| `text.disabled` | `#98A2B3` | Disabled labels | 2.58:1 |
| `accent` | `#2563EB` | Progress, icons, selection bar | 5.17:1 vs white |
| `accent.hover` | `#1D4ED8` | Hover | 6.70:1 |
| `accent.pressed` | `#1E40AF` | Pressed | 8.72:1 |
| `accent.fill` | `#2563EB` | Primary fill (white label) | white 5.17:1 |
| `accent.fill.hover` | `#1D4ED8` | Primary hover | white 6.70:1 |
| `accent.fill.pressed` | `#1E40AF` | Primary pressed | white 8.72:1 |
| `accent.on` | `#FFFFFF` | Label/icon on `accent.fill` in light | 5.17:1 on `#2563EB` |
| `accent.text` | `#1D4ED8` | Links on white | 6.70:1 |
| `accent.soft` | `#E8F0FE` | Selected nav, badges | text 13.9:1 |
| `selection.bg` | `#E8F0FE` | Selected row | text 14.45:1 |
| `selection.bar` | `#2563EB` | 2 px left bar | 4.6:1 vs selection |
| `focus.ring` | `#2563EB` | Focus border | 5.17:1 |
| `success` | `#047857` | Seeding/completed | 5.48:1 |
| `success.soft` | `#E7F6EF` | Fill | — |
| `warning` | `#92400E` | Checking/caution text | 6.46:1 |
| `warning.soft` | `#FEF3E2` | Fill | — |
| `danger` | `#B91C1C` | Error/destructive | 6.47:1 |
| `danger.fill` | `#B91C1C` | Filled destructive (white label) | 6.47:1 |
| `danger.fill.hover` | `#DC2626` | Filled destructive hover | white 4.83:1 |
| `danger.fill.pressed` | `#991B1B` | Filled destructive pressed | white 8.31:1 |
| `danger.on` | `#FFFFFF` | Label/icon on `danger.fill` in light | 6.47:1 |
| `danger.soft` | `#FDECEC` | Fill | — |
| `info` | `#1D4ED8` | Info text | 6.70:1 |
| `info.soft` | `#E8F0FE` | Fill | — |
| `progress.track` | `#E4E7EC` | Track | — |
| `scrollbar.thumb` | `#C4CBD5` | Idle thumb | — |
| `scrollbar.thumb.hover` | `#98A2B3` | Hover thumb | — |

Light is a **secondary** theme. Default follows the OS (`ThemePreference::System`), but the
product screenshots and acceptance tests use dark. The light canvas is deliberately neutral
(`#F5F6F8`), not the bluish `#ECF0F3` sampled from FDM and criticized by its users.

### 2.3 State recipes

State is expressed with a complete recipe, not only a color. `iced` cannot alpha-composite a
state layer inside a stock widget, so each recipe is a concrete token pair.

| State | Recipe (dark) | Recipe (light) | Non-color cue |
|---|---|---|---|
| Default | `bg.surface`; `border.default` | `bg.surface`; `border.default` | — |
| Hover | `bg.raised`; `border.strong` | `bg.overlay`; `border.strong` | cursor pointer |
| Pressed | `bg.overlay`; `border.strong` | `#E4E9F0`; `border.strong` | 1 px inset (translate content 1 px down) |
| Focus | reserve 2 px transparent border, swap to `focus.ring` | same | visible ring, no layout shift |
| Disabled | fill 40% opacity; `text.disabled`; no hover | same | no pointer; tooltip explains why |
| Selected | `selection.bg` + 2 px `selection.bar` left + `text.primary` 600 | same | bar + weight |
| Selected + focus | selection recipe + 2 px `focus.ring` inset | same | ring inside bar |

Selection must never be communicated by background alone: the 2 px bar and 600 weight are
part of the recipe. In the table, the whole row is the hit target.

### 2.4 Typography

**UI family: Inter** (SIL OFL 1.1). Static weights only: Regular 400, Medium 500,
SemiBold 600. Do **not** embed Bold 700; it is too heavy at 13 px and is reserved for nothing
in this UI. Inter's tall x-height keeps 12–13 px table text legible on Windows.

| Token | Size / line height | Weight | Use |
|---|---|---|---|
| `type.display` | 24 / 30 | 600 | Empty-state title, player title |
| `type.title` | 18 / 24 | 600 | Modal title, details torrent name |
| `type.heading` | 15 / 20 | 600 | Section headings, settings group titles |
| `type.body` | 13 / 18 | 400 | Default UI text, table names, form values |
| `type.body.strong` | 13 / 18 | 600 | Selected row name, emphasized values |
| `type.caption` | 12 / 16 | 400 | Table cells, helper text, status bar |
| `type.label` | 12 / 16 | 500 | Field labels, tabs, badges |
| `type.micro` | 11 / 14 | 600 | Sidebar section labels (uppercase), column units |
| `type.mono` | 12 / 16 | 400 | Info hashes only; never speeds or sizes |
| `type.numeric` | 12 / 16 | 500 | Right-aligned table numbers, tabular feel |

Rules:
- Sentence case everywhere except `type.micro` sidebar labels (`LIBRARY`, `SAVE TO`) and
  `PRIVATE`.
- No letter-spacing control is exposed by iced; do not design anything that depends on
  tracking. Uppercase micro labels work because they are short.
- Maximum two weights in one view: 400 + 600. Use 500 only for table numbers and labels.
- Truncate with a trailing ellipsis (`…`) for names, middle ellipsis for paths. Never wrap a
  table cell.
- `text.line_height(LineHeight::Absolute(px))` is set explicitly for every style so row
  heights are deterministic across fonts/DPI.

### 2.5 Spacing

Base unit 4 px. The 2 px step exists only for optical alignment of icons and borders.

| Token | px | Use |
|---|---|---|
| `space.0` | 0 | flush |
| `space.1` | 4 | icon-to-label, badge inset |
| `space.1h` | 2 | optical icon nudge, border inset |
| `space.2` | 8 | button gap, cell gap, list gap |
| `space.3` | 12 | cell padding-x, card gap, toolbar group gap |
| `space.4` | 16 | card padding, settings row padding-y |
| `space.5` | 24 | page gutter, modal body padding, section gap |
| `space.6` | 32 | empty-state gap, large section gap |
| `space.7` | 48 | empty-state icon block, player control gap |

Layout rhythm: 8 px between related controls, 12 px inside a group, 24 px between groups,
32 px between major sections.

### 2.6 Radii, borders, elevation, motion

| Token | Value | Use |
|---|---|---|
| `radius.xs` | 4 | badges, checkbox, small chips, tooltips |
| `radius.sm` | 8 | inputs, dropdowns, icon buttons, menus, nav selection |
| `radius.md` | 12 | modals, sheets, drop zone, cards, player overlay |
| `radius.pill` | 999 | primary/secondary CTAs, segmented control, state pills |
| `border.hairline` | 1 px | default outline, dividers |
| `border.focus` | 2 px | keyboard focus; reserved transparent in default state |
| `border.accent-bar` | 2 px | selected row/nav left bar |
| `border.dashed` | 2 px dashed | drop zone (1 px at rest, 2 px on drag-over) |

Elevation:
- **Dark:** in-flow surfaces never use shadows. Use `bg.surface` → `bg.raised` → `bg.overlay`
  steps and 1 px borders. Only floating layers get shadows:
  menu/popover `0 8px 24px rgba(0,0,0,0.45)`;
  modal `0 24px 64px rgba(0,0,0,0.60)`;
  toast `0 12px 32px rgba(0,0,0,0.50)`.
- **Light:** in-flow cards are flat with `border.default`; menus use
  `0 8px 24px rgba(16,24,40,0.14)`; modals `0 24px 48px rgba(16,24,40,0.18)`.
- `iced::Shadow` approximates CSS shadows with `offset`, `blur_radius`, `color`; the values
  above are targets, not pixel-exact CSS equivalents. Never put a shadow on a table row.

| Motion token | Duration | Easing | Use |
|---|---|---|---|
| `motion.instant` | 0 ms | — | hover/press on stock widgets (iced has no CSS transitions) |
| `motion.fast` | 120 ms | ease-out | progress value interpolation, icon color |
| `motion.base` | 180 ms | ease-out | details pane expand/collapse, tab underline |
| `motion.overlay` | 200 ms | ease-out | modal opacity + 8 px translate |
| `motion.toast` | 250 ms in / 200 ms out | ease-out | toast enter/exit |

Reduced motion sets all durations to 0 and removes translate/scale; only opacity changes are
allowed. There is no OS reduced-motion API in iced 0.14; expose an in-app toggle in
Appearance and default it to **on** (no decorative motion) until OS detection is added.

### 2.7 Iconography

**Lucide** (ISC), 24×24 viewBox, 2 px stroke, round caps/joins. Render at 16, 18, or 20 px;
never stretch. At 16 px a 2 px stroke becomes 1.33 px, so 16 px is reserved for inline
metadata; toolbar and nav icons use 18–20 px. Do not mix filled and outline icons. Selected
state changes color/weight of the container, not the icon fill.

Required set (≈40): `plus`, `magnet`, `search`, `x`, `play`, `pause`, `square`, `trash-2`,
`folder-open`, `settings`, `chevron-down`, `chevron-right`, `more-horizontal`, `arrow-down`,
`arrow-up`, `arrow-down-up`, `clock`, `check`, `circle-check`, `alert-triangle`, `info`,
`lock`, `globe`, `link`, `file`, `file-text`, `film`, `music`, `subtitles`, `hard-drive`,
`download`, `upload`, `list-filter`, `columns-3`, `grip-vertical`, `maximize`, `minimize`,
`volume-2`, `rotate-cw`, `wifi-off`.

Sizes: 14 inline (badges), 16 table/state, 18 buttons/nav, 20 toolbar, 24 empty-state
secondary, 32 drop-zone, 48 empty-state hero. Icon color follows the text token of its
context; active nav icon uses `accent`; destructive uses `danger`.

---

## 3. Layout anatomy

### 3.1 Window chrome — native (recommended)

Use **native Windows decorations** (`window::Settings { decorations: true }`). Reasons:
Snap Layouts, system menu, high-contrast themes, DPI/resize behavior, and accessibility all
come for free. FDM draws a custom title bar, but reproducing it safely in iced requires
hit-testing the non-client area and reimplementing snap; that is a P2 experiment, not a P0
requirement.

```rust
window::Settings {
    size: Size::new(1280.0, 800.0),
    min_size: Some(Size::new(960.0, 600.0)),
    resizable: true,
    decorations: true,
    exit_on_close_request: false, // save UI state, then close
    ..window::Settings::default()
}
```

Title: `Oxide Flux` (fixes the current `Flux App - Iced` default). Window icon: 256×256 RGBA
generated from the app mark; no FDM/qBittorrent artwork. The header inside the client area is
**not** a drag region because chrome is native.

### 3.2 1280×800 at 100% (client area 1280×768 with a 32 px title bar)

```
 y=0    ┌──────────────────────────────────────────────────────────────────────┐
        │ native title bar: Oxide Flux                                  – □ ×  │ 32
 y=32   ├──────────────────────────────────────────────────────────────────────┤
        │ HEADER                                                           56  │
 y=88   ├──────────────────────────────────────────────────────────────────────┤
        │ TOOLBAR                                                          44  │
 y=132  ├────────────────┬─────────────────────────────────────────────────────┤
        │ LEFT NAV       │ CONTENT                                             │
        │ 232 fixed      │ 1048 fill                                            │
        │                │  ┌ table header 32                                  │
        │                │  ├ rows 32 each                                     │
        │                │  └ drop strip 56 (non-empty + details collapsed)    │
        │                ├─────────────────────────────────────────────────────┤
        │                │ DETAILS PANE 232 (open) / 40 (collapsed tab strip)  │
 y=740  ├────────────────┴─────────────────────────────────────────────────────┤
        │ STATUS BAR                                                       28  │
 y=768  └──────────────────────────────────────────────────────────────────────┘
```

Exact regions (dark theme):

| Region | Position / size | Contents | Resize behavior |
|---|---|---|---|
| Native title bar | 0–32 | OS controls | OS |
| Header | 32–88, h 56, pad-x 24 | Brand 24 + wordmark 15/600 + version 11; search 320×32; Add split 120×36; overflow 32 | Search shrinks to 240 at <1200; collapses to icon at <1080 |
| Toolbar | 88–132, h 44, pad-x 24 | Pause / Resume / Remove (icon 18 + label 13); right: sort menu, Columns | Labels hide at <1150; icons only with tooltips |
| Left nav | 132–740, w 232, pad 12 | Library groups + counts; Save to block pinned bottom | 232 → 200 at <1200 → 64 rail at <1080; hidden under 960 |
| Content | x 232, y 132–508 | toolbar strip 40, header 32, rows 32 | Width fills; min 640 |
| Drop strip | bottom of content, h 56 | dashed 1 px, icon 16, “Drop .torrent or magnet link” | Hidden when details open or list empty (empty state owns the drop zone) |
| Details pane | y 508–740, h 232 | identity header 40 + tabs 36 + content 156 | Drag handle 6 px; 40 collapsed, 160 min, 400 max |
| Status bar | 740–768, h 28, pad-x 16 | counts, DHT state, ↓/↑, session ratio, selection | Always visible; text truncates |

With details open (232 px) the table body is **304 px ≈ 9 rows** after the 40 px content
header and 32 px table header; the 56 px drop strip is hidden while details are open. With
details collapsed (40 px) the body is **440 px ≈ 13 rows** including the drop strip. This is
the cost of a bottom details pane; it is the right trade at 1280 because a right-side pane
would steal 360 px from the table. Users who want more rows collapse details or choose
Compact density (28 px rows → 10 rows open).

Vertical stack implementation: `column![header, toolbar, body, status_bar]`; `body` is a
`row![nav, content]`; `content` is a `pane_grid` with two vertical panes (table, details) so
the splitter is stock. Do not hand-roll drag math.

### 3.3 Resize rules

| Window width | Nav | Table columns | Header |
|---|---|---|---|
| ≥1600 | 240 fixed | All default + optional columns | Search 360 |
| 1280–1599 | 232 | Default 8 columns | Search 320 |
| 1100–1279 | 200 | Hide ETA, then Up | Search 280 |
| 960–1099 | 64 icon rail | Hide Size, then Up, ETA; keep Name/Progress/Status/Down/Peers | Add becomes icon-only 36 |
| <960 | overlay drawer | Horizontal scroll; min 640 content | Search icon-only |

Column hide order (last to be hidden): **Name → Progress → Status → Down → Peers → Size → Up
→ ETA**. Never hide Name, Progress, or Status. At <960 the table scrolls horizontally instead
of hiding more.

Height: header 56, toolbar 44, status 28 are fixed. The body absorbs all remaining height.
Details pane is clamped to `min(400, body_height - 160)`. At heights <640, details default to
collapsed.

### 3.4 Density

| Density | Row height | Header height | Toolbar | Use |
|---|---|---|---|---|
| Comfortable (default) | 32 | 32 | 44 | FDM-like breathing room |
| Compact | 28 | 28 | 40 | qBittorrent-like libraries, >2,000 torrents |

Density is a user setting in Appearance and persists. Do not go below 28: 24 px rows make
13 px text and 16 px state icons collide, and the hit target falls below comfortable desktop
use. The table remains readable at 28 because text is 12/16 and padding-y is 6.

Appearance also offers **Hide zero speeds** (default off). When on, an idle row shows `—` in
Down/Up instead of `0 B/s`; sorting still treats the value as 0, and a nonzero rate is never
hidden. This addresses FDM's “hundreds of 0 B/s” complaint without lying about the data.

---

## 4. Component specifications

Notation: `D` = default, `H` = hover, `P` = pressed, `F` = focus, `X` = disabled,
`S` = selected. All heights include borders. All controls reserve a 2 px transparent border
so focus does not shift layout.

### 4.1 Buttons

| Variant | Height | Pad-x | Radius | D | H | P | X |
|---|---|---|---|---|---|---|---|
| Primary | 36 (large 40) | 16 | pill | `accent.fill`, `accent.on` 13/600, 1 px `accent.fill.hover` at 35% | `accent.fill.hover`, `accent.on` | `accent.fill.pressed`, `accent.on` | 40% fill, `text.disabled`, no hover |
| Secondary | 32 | 14 | pill | `bg.raised`, 1 px `border.default`, `text.primary` | `bg.overlay`, `border.strong` | `bg.overlay` + content 1 px down | 40%, `text.disabled` |
| Ghost | 32 | 12 | 8 | transparent, `text.secondary` | `bg.overlay`, `text.primary` | `bg.overlay` + 1 px down | `text.disabled` |
| Danger (text) | 32 | 12 | 8 | transparent, `danger` | `danger.soft`, `danger` | `danger.soft` darkened 6% | 40% |
| Danger (filled) | 36 | 16 | pill | `danger.fill`, `danger.on` | `danger.fill.hover` | `danger.fill.pressed` | 40% |
| Icon | 32×32 | 0 | 8 | transparent, `text.secondary`, icon 18 | `bg.overlay`, `text.primary` | `bg.overlay` | `text.disabled` |
| Split primary | 120×36 | 12 | pill | primary label + 1 px divider + chevron 16 | segments highlight independently | — | 40% |

- Primary label is 13/600; secondary/ghost 13/500. Icon-to-label gap 8.
- Focus: swap the reserved 2 px border to `focus.ring`; for pill buttons the ring follows the
  pill radius.
- Selected toggle (e.g. “Sequential”): `accent.soft` fill, 1 px `accent`, `accent.text`, icon
  `accent`.
- Disabled must carry a tooltip reason when the reason is not obvious (“Streaming needs
  sequential download support”, “No playable file”).
- No button uses the old purple default. CI gate in §8.

### 4.2 Segmented filter

Used in Add sheet (Torrent file / Magnet link) and Appearance (System / Dark / Light).
Container 32 high, radius pill, `bg.overlay`, 1 px `border.default`, 2 px inner padding.
Segments 28 high, pad-x 12, 12/500, radius pill, gap 0.
D: `text.secondary`; H: `bg.raised`, `text.primary`; S: `accent.fill`, `accent.on` 600; F: 2 px
`focus.ring` inset; X: 40% and no hover. Thumb has no sliding animation in P0; swap instantly.

### 4.3 Sidebar item

```
┌──────────────────────────────────┐
│  ▸  All                    24    │  34 h, 12 pad-x, 8 gap
└──────────────────────────────────┘
   ↑ 2 px accent bar when selected
```

| Property | Value |
|---|---|
| Height | 34 (compact 30) |
| Pad-x | 12 |
| Radius | 8 |
| Icon | 18, `text.secondary` |
| Label | 13/500, `text.primary` |
| Count | 11/600, `text.muted`; 18 high pill, min-w 22, `bg.overlay`; 0 is muted, >0 secondary |
| D | transparent |
| H | `bg.overlay` |
| S | `selection.bg`, 2 px `selection.bar`, label 600, icon `accent`, count `accent.text` |
| F | 2 px `focus.ring` inset |
| X | label `text.disabled`, count hidden |

Section header (`LIBRARY`, `SAVE TO`): 11/600 uppercase, `text.muted`, 24 px top margin,
8 px bottom, pad-x 12. Groups: Library (All, Downloading, Seeding, Completed, Paused,
Checking, Error), Categories, Tags, Trackers. **Only render a group when its data exists**
(see §7.1); an empty “Categories” header is worse than no header.

Collapsed rail (64 px): 40×40 icon buttons, icon 20, count as a 16 px badge at top-right;
tooltip on hover. Save-to block becomes a hard-drive icon with path tooltip.

### 4.4 Table header

| Property | Value |
|---|---|
| Height | 32 (compact 28) |
| Background | `bg.surface` |
| Bottom border | 1 px `border.strong` |
| Text | 12/500, `text.muted`, sentence case |
| Pad-x | 12 |
| Sort icon | 12, `text.muted`; active `accent`; 4 px gap |
| Resize handle | 4 px hit area, cursor `col-resize`, hover 1 px `accent` |
| Focus | header cell 2 px `focus.ring`; Enter toggles sort |

States: D `bg.surface` / `text.muted`; H `bg.raised` / `text.secondary`; P `bg.overlay`;
F 2 px `focus.ring` inset; S (active sort) `text.primary` + `accent` chevron; X not applicable.

Click cycles `asc → desc → default`. Show `chevron-up` / `chevron-down` only for the active
column. Default sort is engine order until `added_at` exists; then “Added” descending. A
second click on another header replaces the sort (multi-sort is P2). Column reorder by drag is
P2; column visibility is a menu (`Columns` icon) in the toolbar.

### 4.5 Table row

| Property | Value |
|---|---|
| Height | 32 (compact 28) |
| Background | `bg.surface` |
| Separator | 1 px `border.subtle` (bottom) |
| Pad-x | 12 |
| Name | 13/500, `text.primary`, trailing ellipsis, 1 line |
| Numeric | 12/500, `text.secondary`, right-aligned |
| State | icon 16 + 12/500 label; label `text.secondary`, icon state color |
| Progress | 72 px bar + 40 px `72%` (12/500), 8 px gap |
| Hover | `bg.raised` |
| Selected | `selection.bg`, 2 px `selection.bar`, name 13/600 |
| Focus | 2 px `focus.ring` inset (only when keyboard focus is on the table) |
| Disabled cell | `—`, `text.muted`, tooltip “Not reported by this engine” |
| X (torrent removed) | row fades to 40% for 120 ms, then leaves (P2); P0 removes instantly |

Row actions: hover reveals a 28×28 `more-horizontal` icon button at the far right, 8 px from
the edge; double-click anywhere else opens details. The whole row is a button, so it is
keyboard-focusable; the row action button is a separate focus stop.

State cell mapping (icon 16 + label 12/500):

| State | Icon | Icon color | Label |
|---|---|---|---|
| `Queued` | `clock` | `info` | Queued |
| `Checking` | `rotate-cw` | `warning` | Checking |
| `Downloading` | `arrow-down` | `accent` | Downloading |
| `Seeding` | `arrow-up` | `success` | Seeding |
| `Complete` | `circle-check` | `success` | Completed |
| `Paused` | `pause` | `text.muted` | Paused |
| `Error` | `alert-triangle` | `danger` | Error |

Zebra striping is an Appearance option, default **off**. When on, odd rows use `bg.overlay` at
50% (`#181C22` dark, `#FAFBFC` light). Do not combine zebra with the selected background.

### 4.6 Progress bar

**Thin (table, 6 px):** track `progress.track`, fill `accent`, radius pill, 6 px high, 72 px
wide. At 100% fill `success`; paused fill `text.muted`; error fill `danger`; checking fill
`warning` at the reported progress. If checking progress is unknown, draw a 2 px `warning` cap
at the left and the label “Checking…” — never a fake indeterminate animation. Percent label sits outside, 12/500
`text.secondary`; at 100% label `success`; at error `danger`.

**Labelled (details header, 8 px):** 8 px bar + 12/500 label above-right (“72% · 8.4 MB/s ·
ETA 12m”). The bar is full width of the details header, max 360 px. Do not put text inside a
6 px bar.

No shimmer, stripes, or glow. The bar is a measurement, not decoration. Progress interpolation
(120 ms) is allowed only when the value changes by <5 percentage points; jumps are instant.

### 4.7 Tabs

Details tabs: General, Files, Peers, Trackers. Height 36, pad-x 12, 12/500 `text.secondary`;
selected `text.primary` 600 + 2 px `accent` bottom bar; hover `bg.overlay`; focus 2 px
`focus.ring` inset; disabled `text.disabled` with tooltip. No pill tabs here; pills are for
filters. Tabs scroll horizontally at <720 px content width; overflow chevrons 24 px.

Settings nav uses sidebar items (§4.3), not tabs. Add sheet uses segmented control (§4.2).

### 4.8 Search field

| Property | Value |
|---|---|
| Size | 320×32 (280 at <1280, icon-only at <1080) |
| Radius | 8 |
| Background | `bg.overlay` |
| Border | 1 px `border.default`; focus 2 px `accent` |
| Leading icon | `search` 16, `text.muted`, 10 px left inset |
| Text | 13/400, `text.primary`; placeholder 13/400 `text.muted` |
| Clear | `x` 14 in a 20×20 button, only when non-empty |
| Shortcut | `Ctrl+F` focuses; `Esc` clears then blurs |

Typing filters the table after 150 ms debounce, case-insensitive, matches name and (when
available) category/tags/tracker. Show a result count in the content header (“8 of 24”) and a
“Clear search” action in the no-results state. The search field never filters the sidebar
counts; it shows the filtered count next to the active filter.

### 4.9 Text input / textarea / dropdown

Text input 32 high (forms 36), radius 8, `bg.raised`, 1 px `border.input`, pad-x 12, 13/400.
Placeholder `text.muted`. Focus 2 px `accent` + no shadow. Error 1 px `danger` + 12 px message
below, icon 14. Disabled `bg.overlay`, `text.disabled`, no border change.

Textarea (magnet list) 72 high, same tokens, 12/16 mono optional; resize handle none. Validate
on submit, not on every keystroke; show a single inline error.

Dropdown (`pick_list`) 32 high, radius 8, `bg.raised`, 1 px `border.input`, pad-x 12, chevron
14. Menu: `bg.raised`, 1 px `border.default`, radius 8, shadow, 4 px padding, item 32, selected
item `accent.soft` + check 14, hover `bg.overlay`, max height 280 with scroll. Keyboard:
Up/Down, Enter, Esc.

### 4.10 Checkbox / radio / toggle

| Control | Size | D | S | F | X |
|---|---|---|---|---|---|
| Checkbox | 18×18, radius 4 | `bg.raised`, 1 px `border.input` | `accent.fill`, `accent.on` `check` 12 | 2 px `focus.ring` outside 2 px gap | 40%, no hover |
| Radio | 18×18 circle | `bg.raised`, 2 px `border.input` | `accent.fill` ring + 6 px `accent.on` dot | 2 px `focus.ring` | 40% |
| Toggle | 36×20, pill | `bg.overlay`, 1 px `border.input`, knob 16 `text.secondary` | `accent.fill`, knob 16 `accent.on`, 2 px inset | 2 px `focus.ring` | 40% |

Label 13/400, 8 px gap, whole row is a hit target (min 32 high). Checkbox tri-state for
folders: checked, indeterminate (2 px `accent` dash), unchecked. Toggle is used only for
immediate settings (`Start when added`, `DHT`); checkbox is used when the change is part of a
form and applied on submit.

### 4.11 Badges

| Badge | Height | Radius | Fill / text | Content |
|---|---|---|---|---|
| State | 22 | pill | `<state>.soft` / `<state>` | icon 12 + 11/600 label |
| Private | 20 | 4 | `bg.overlay` / `text.secondary` | `lock` 12 + `PRIVATE` 10/600 |
| Ratio | 20 | 4 | transparent / `success` if ≥1.0 else `text.secondary` | `1.42` 11/600 |
| Count | 18 | pill | `bg.overlay` / `text.secondary` | number 11/600 |
| New | 18 | 4 | `accent.soft` / `accent.text` | `NEW` 10/600 |

Badges never carry the only copy of a state: the state column also has a label. Ratio is text,
not a badge, in the table (badges in the details header only).

### 4.12 Empty state

Full content area, centered column, max width 560.

- Hero icon 48 in a 72 circle `accent.soft`, icon `accent`.
- Title 24/600 `text.primary`: “No torrents yet”.
- Body 13/400 `text.secondary`, max 44 ch: “Add a .torrent file, paste a magnet link, or drop
  files anywhere in this window.”
- Actions: Primary “Add .torrent” 36, Secondary “Add magnet” 36, 12 gap.
- Dashed drop zone: 100%×180 (min 320), 1 px dashed `border.strong`, radius 12, `bg.raised`;
  icon `download` 32 `text.muted`, line 1 15/600 “Drop .torrent files here”, line 2 12/400
  “or paste a magnet link”; drag-over: 2 px `accent` dashed, `accent.soft` fill, icon
  `accent`, text `accent.text`.
- Footer hint 11/400 `text.muted`: “Torrents stay on this device. Nothing is uploaded unless
  you seed.”

No-results variant: no hero circle, title 18/600 “No torrents match your filters”, body 13,
action “Clear filters” secondary. No drop zone.

### 4.13 Toast and inline error

Toast: bottom-right, 16 px margin, min 320 / max 420 wide, min 48 high, radius 8, `bg.raised`,
1 px `border.default`, shadow, icon 16, message 13/400, close 24. Error toasts persist until
dismissed; success/info auto-dismiss after 5 s. Max 3 stacked, 8 px gap, newest at bottom.
Motion: 250 ms slide+fade in, 200 ms fade out (disable under reduced motion).

Inline error banner: full content width, 40 high, `danger.soft`, 3 px `danger` left border,
icon 16 `danger`, message 12/400 `text.primary`, action “Retry” 12/600 `danger`, close 24.
Used above the table when the engine stops and inside the Add sheet for parse failures.
Engine-stopped banner copy: “Engine stopped. Your torrents are safe on disk.” + “Reconnect”.

### 4.14 Modal sheet

Scrim `rgba(0,0,0,0.55)` covers the client area but not the native title bar. Card: max 760
wide (Settings 900), max 90% of client height, `bg.raised`, radius 12, 1 px `border.default`,
modal shadow. Header 56 (pad-x 24, title 18/600, close icon 32). Body pad 24, scrollable.
Footer 64, top border 1 px `border.subtle`, right-aligned actions 8 gap, primary on the right.
Esc closes; click scrim closes only when there are no unsaved changes; otherwise a small
“Discard changes?” confirm. Focus is trapped; on close, focus returns to the trigger. Enter
activates the default action unless focus is in a multiline field.

### 4.15 Context menu

Width min 220, max 320; `bg.raised`, 1 px `border.default`, radius 8, shadow; 4 px padding.
Item 32, pad-x 10, icon 16, label 13/400, shortcut 11/400 `text.muted` right-aligned; hover
`bg.overlay`; disabled `text.disabled`; danger item `danger` text, hover `danger.soft`.
Separator 1 px `border.subtle`, 4 px vertical margin. Opens at cursor, flips at viewport
edges, keyboard navigable (Up/Down/Enter/Esc), closes on outside click. Submenus open after
120 ms hover or Right arrow. No native menus; implement as a positioned overlay.

**Torrent context menu (only supported items):**

```
Play                         (if a playable file exists)
Pause / Resume               Space
Open folder
─────────────────────────────
Copy magnet link
Copy info hash               (only when available)
Copy name
─────────────────────────────
Download in sequential order ☐   (disabled when finished/unsupported)
─────────────────────────────
Remove…                      Del
Remove and delete files…     Shift+Del   (danger)
Properties                   Enter
```

Not present until the engine supports them: Force start, Recheck, Reannounce, Super seeding,
Category, Tags, Queue priority, Bandwidth priority. Do not add greyed-out decorations for
features that may never exist; a long dead menu is worse than a short live one.

### 4.16 Tooltip and scrollbar

Tooltip: 28 high, pad-x 8, radius 4, `bg.overlay`/`#2A3039` dark, `text.primary`, 11/400,
1 px border, shadow; max 280 wide, 2 lines. Delay 500 ms on hover, 0 on keyboard focus;
dismiss on press/Esc. Contrast ≥7:1; never yellow (FDM's dark tooltip complaint). P0 uses the
stock `tooltip` (immediate); the delayed wrapper is P1.

Scrollbar: 10 px rail, 6 px thumb inset 2 px, radius pill, `scrollbar.thumb`; hover
`scrollbar.thumb.hover`; track transparent. Visible whenever content overflows (not
auto-hiding). Table uses its own vertical scrollbar at the right edge of the content, inside
the 1 px border; details pane and sidebar each have independent scrollbars.

---

## 5. Screen blueprints

The ASCII sketches show hierarchy and content, not exact pixel widths; the token and layout
tables in §2–§4 are authoritative. Every state shown is one of the component states in §4.

### 5.1 (a) Library / All with torrents

```
┌────────────────────────────────────────────────────────────────────────────────┐
│ ◆ Oxide Flux  v0.1.0        ⌕ Search torrents…          + Add ▾        ⋯       │ 56
├────────────────────────────────────────────────────────────────────────────────┤
│ ⏸ Pause   ▶ Resume   🗑 Remove            ⇅ Name ⌄     ▦ Columns     ⌕ filter  │ 44
├──────────────────┬─────────────────────────────────────────────────────────────┤
│ LIBRARY          │ All torrents   24                            8.2 MB/s ↓      │ 40
│ ▸ All        24  ├─────────────────────────────────────────────────────────────┤
│   Downloading 3  │ ☐ Name                 Size   Progress    Status  Peers ↓ ↑  │ 32
│   Seeding    16  │ ─────────────────────────────────────────────────────────── │
│   Completed  18  │ ☐ Cosmos…2015.mp4    1.2 GB  ███████░░ 72% ↓ 6.4 MB/s  14  │ 32
│   Paused      1  │ ☐ Ubuntu-24.04.iso   2.8 GB  ██████████ 100% Seeding    22  │ 32
│   Checking    0  │ ☐ Tears-of-Steel.mkv 3.7 GB  ███░░░░░░ 31% ↓ 1.8 MB/s   9  │ 32
│   Error       0  │ ☐ Debian-12.5.iso    3.1 GB  ██████████ 100% Completed  18  │ 32
│                  │ ☐ Sintel.2010.4k.mkv 5.2 GB  ██████░░░ 64% Paused        0  │ 32
│                  │ …                                                           │
│ ───────────────  │                                                             │
│ SAVE TO          │                                                             │
│ D:\Downloads     │                                                             │
│ 412 GB free      │                                                             │
├──────────────────┴─────────────────────────────────────────────────────────────┤
│ Cosmos…2015.mp4  [General] [Files] [Peers] [Trackers]                      ⌃  │ 40
│ Size 1.24 GB   Pieces 298/412   Save path D:\Downloads\…   Private  No         │ 196
├────────────────────────────────────────────────────────────────────────────────┤
│ 24 torrents · 3 active · 16 seeding · 1 paused   DHT on   ↓ 8.2 MB/s ↑ 1.4 MB/s│ 28
└────────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- Counts in the sidebar include the active filter’s own count and are recomputed only on
  snapshot. “Completed” includes both `Seeding` and `Complete`; “Seeding” is separate.
- The drop strip is 56 high and only appears when the list is non-empty and details are
  collapsed. It is a drop target, not a button; clicking it opens the Add sheet.
- The details pane header doubles as the torrent identity: name 15/600, state badge, 8 px
  progress bar, close/collapse chevron. Tabs are 36 high.
- The status bar never shows DHT node count (not exposed). “DHT on” is from config; hover
  shows “Mainline DHT enabled. Node count is not reported by this engine.”
- `412 GB free` is a UI-side filesystem stat on the save path, not engine data. It is P1;
  hide the line until it is implemented and never guess it.
- The table’s right edge aligns with the content padding, not the window edge.

### 5.2 (b) Empty state with dashed drop zone

```
┌────────────────────────────────────────────────────────────────────────────────┐
│ ◆ Oxide Flux  v0.1.0        ⌕ Search torrents…          + Add ▾        ⋯       │
├────────────────────────────────────────────────────────────────────────────────┤
│ ⏸ Pause   ▶ Resume   🗑 Remove            ⇅ Name ⌄     ▦ Columns              │
├──────────────────┬─────────────────────────────────────────────────────────────┤
│ LIBRARY          │                                                             │
│ ▸ All         0  │                    ╭──────────────╮                         │
│   Downloading 0  │                    │   ⇣  icon    │                         │
│   Seeding     0  │                    ╰──────────────╯                         │
│   Completed   0  │                  No torrents yet                           │
│   Paused      0  │      Add a .torrent file, paste a magnet link, or           │
│   Checking    0  │           drop files anywhere in this window.               │
│   Error       0  │                                                             │
│                  │          [ + Add .torrent ]   [ Add magnet ]                │
│                  │                                                             │
│ ───────────────  │   ┌───────────────────────────────────────────────────────┐ │
│ SAVE TO          │   │                                                       │ │
│ D:\Downloads     │   │        ⇣  Drop .torrent files here                    │ │
│ 412 GB free      │   │           or paste a magnet link                      │ │
│                  │   └───────────────────────────────────────────────────────┘ │
├──────────────────┴─────────────────────────────────────────────────────────────┤
│ 0 torrents · 0 active · 0 seeding · 0 paused          DHT on   0 B/s   0 B/s   │ 28
└────────────────────────────────────────────────────────────────────────────────┘
```

Notes: the drop zone is 100% width × 180, 32 px below the actions. The sidebar still shows
zero counts (they are informative), but no Categories/Tags/Trackers sections. Pause/Resume/
Remove are disabled with no selection; the toolbar stays visible so the layout does not jump.
The status bar shows `0 B/s`, not `—`, because zero is measured. The empty state disappears on
the first torrent; there is no separate “welcome” route.

### 5.3 (c) Add torrent / magnet sheet

```
        ┌──────────────────────────────────────────────────────────────┐
        │  Add torrent                                            ×    │ 56
        ├──────────────────────────────────────────────────────────────┤
        │  [ Torrent file ] [ Magnet link ]                            │ 32
        │                                                              │
        │  ┌────────────────────────────────────────────────────────┐  │
        │  │   ⇣   Drop a .torrent file here                        │  │ 140
        │  │       or click to browse                               │  │
        │  └────────────────────────────────────────────────────────┘  │
        │                                                              │
        │  name          Cosmos.Laundromat.2015.1080p.mkv              │ 44
        │  size          1.24 GB · 412 pieces · 4 MiB pieces           │ 20
        │  save to       D:\Downloads\OxideFlux            [ Browse ]  │ 44
        │                                                              │
        │  Files  298 selected · 1.24 GB          [All] [None] [Video] │ 32
        │  ┌────────────────────────────────────────────────────────┐  │
        │  │ ▾ ☑ Cosmos.Laundromat.2015.1080p.mkv   1.18 GB  Normal │  │ 28
        │  │   ☑ Cosmos.Laundromat.2015.1080p.srt      42 KB  Normal │  │ 28
        │  │   ☐ Sample.mkv                            18 MB  Skip   │  │ 28
        │  └────────────────────────────────────────────────────────┘  │
        │                                                              │
        │  ☑ Start when added     ☐ Download in order (stream)         │ 32
        ├──────────────────────────────────────────────────────────────┤
        │  1.24 GB · 2 of 3 files              [ Cancel ] [ Add torrent ]│ 64
        └──────────────────────────────────────────────────────────────┘
```

Notes:
- One sheet, progressive disclosure: source → metadata → files. No wizard.
- Magnet tab replaces the drop zone with a 36 px input + “Fetch” secondary button; while
  fetching, show a 28 px spinner, “Fetching metadata… 7 peers”, and a Cancel action. No fake
  file names or sizes before metadata arrives; skeleton rows are unlabeled grey blocks.
- File rows 28 high; folder rows 28; checkbox 18; priority is a 92×24 `pick_list` with
  `Skip / Normal / High` only. `Low` is not offered (engine returns `Unsupported`).
- “Video” filter selects files with a playable extension (the same list as `player::is_media`).
  “Download in order” is disabled until at least one playable file is selected, with tooltip.
- Footer count updates live; primary disabled when zero files are selected.
- Duplicate torrent → inline warning with “Open existing” action, not a modal-on-modal.

### 5.4 (d) Torrent details — Files tab active

```
┌────────────────────────────────────────────────────────────────────────────────┐
│ Cosmos…2015.mp4   ● Downloading   ███████░░░ 72% · 6.4 MB/s · ETA 12m     ⌃   │ 64
│ [General] [Files] [Peers] [Trackers]                                           │ 36
├────────────────────────────────────────────────────────────────────────────────┤
│ Files  298 selected · 1.18 GB of 1.24 GB        [All] [None] [Playable]        │ 32
│ ☐ Name                                        Size      Priority     Action    │ 28
│ ▾ ☑ Cosmos.Laundromat.2015.1080p.mkv          1.18 GB   Normal       ▷ Play*   │ 28
│   ☑ Cosmos.Laundromat.2015.1080p.srt            42 KB   Normal                 │ 28
│   ☐ Sample.mkv                                  18 MB   Skip                   │ 28
│   ☑ Extras/Behind the scenes.mp4               84 MB   High         ▷ Play*   │ 28
├────────────────────────────────────────────────────────────────────────────────┤
│ Peers tab columns:  Address | Client | Progress | Down | Up | Flags            │
│ Trackers tab:       URL | Status | Seeds | Peers | Next announce | Message     │
└────────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- The identity header is 40 high when expanded or collapsed. It contains the 8 px progress bar
  and the state badge; the tab strip below is 36.
- Files tab is a tree with 28 px rows, indent 16 per level, chevron 14. Columns: Name (flex),
  Size (88 right), Priority (110), Action (88). No per-file progress column: the engine's
  `FileView` has no per-file byte count. If a per-file progress is added, insert it before
  Priority.
- `Play*` is disabled in this blueprint: the torrent is 72% and the engine cannot stream yet.
  Play is enabled only for playable files, and today only at 100%; when streaming lands, it is
  enabled when the file has contiguous buffered data (see §7.4). Disabled Play has tooltip
  “Streaming playback needs sequential download support”.
- Peer and tracker tabs are **API-gated**: `TorrentDetail` currently exposes neither list.
  Ship them only when the fields exist; until then, do not render the tabs. If a tab must be
  shown, use the unavailable state: centered 16 icon, “This engine build does not report
  peers.”, 12/400 muted, no table header.
- General tab is a two-column key/value grid, label 12/400 `text.muted`, value 13/400
  `text.primary`, row 28, column gap 24. Available rows: Size, Progress, Pieces, Piece length,
  Save path, State, Peers, Down, Up, ETA, Private. Derived/estimated values are not shown.

### 5.5 (e) Settings dialog

```
        ┌────────────────────────────────────────────────────────────────┐
        │  Settings                                                  ×   │ 56
        ├──────────────────┬─────────────────────────────────────────────┤
        │  General         │  APPEARANCE                                 │
        │  Downloads       │  Theme          [System][Dark][Light]       │ 56
        │  Connection      │  Density        [Comfortable ▾]             │ 56
        │  Appearance    ● │  Reduce motion  [ toggle ]                  │ 56
        │                  │  Hide zero speeds [ toggle ]                 │ 56
        │  Advanced        │                                             │
        │  About           │  ─────────────────────────────────────────  │
        │                  │  DOWNLOADS                                  │
        │                  │  Save to        D:\Downloads\OxideFlux [..] │ 56
        │                  │  Preallocate    [Sparse ▾]       restart ●  │ 56
        │                  │  Max downloads  [ 4 ]  Max seeds [ 8 ]      │ 56
        │                  │                                             │
        │                  │  ─────────────────────────────────────────  │
        │                  │  CONNECTION                                 │
        │                  │  Listen port    [ 0 ]  (0 = random)         │ 56
        │                  │  Max peers      [ 50 ]                      │ 56
        │                  │  DHT            [ toggle ]                  │ 56
        ├──────────────────┴─────────────────────────────────────────────┤
        │  Restore defaults                         [ Cancel ] [ Save ]  │ 64
        └────────────────────────────────────────────────────────────────┘
```

Notes:
- 900×640, left nav 220, content 640, body scrolls. Group header 11/600 uppercase muted,
  24 top / 8 bottom. Row 56: label 13/500, optional description 12/400 muted below, control
  right-aligned. Divider 1 px `border.subtle` between groups, not between every row.
- “restart ●” is a 11/500 `warning` badge with tooltip “Applies after the engine restarts.”
  Settings that need a restart: save path, preallocate, port, peers, slots, DHT, threads,
  persist. Settings that are live: theme, density, reduced motion, hide zero speeds. Do not
  offer a setting that has no effect in this build.
- Advanced groups: persist session, verifier threads, worker threads, state file path
  (read-only), API bind/token (masked, copy), “Open config folder”. About groups: version,
  engine/API version, licenses, repository link.
- Save writes config, shows a toast “Settings saved”, and restarts the in-process engine only
  if a restart-required value changed. Cancel discards; Esc prompts if dirty.
- About: version, engine version, API version, “Licenses” button (Lucide ISC, Inter OFL,
  iced MIT), and a link to the project repository. No FDM/qBittorrent branding.

### 5.6 (f) Player overlay / embedded mpv area

```
┌────────────────────────────────────────────────────────────────────────────────┐
│  ←  Cosmos.Laundromat.2015.1080p.mkv        Streaming · 62%     [−] [□] [×]     │ 48
│                                                                                │
│                                                                                │
│                              ▶   (64 circle)                                   │
│                                                                                │
│                                                                                │
│  ┌──────────────────────────────────────────────────────────────────────────┐  │
│  │ ●───────────────────────────────○───────────────────────────────────────│  │ 4
│  │ buffered ████████████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ │  │ 4
│  └──────────────────────────────────────────────────────────────────────────┘  │
│  ▶  01:24:10 / 02:14:32      🔊 ─────●────   CC   ⛶        Buffered 45s       │ 56
└────────────────────────────────────────────────────────────────────────────────┘
```

Notes:
- The overlay covers the content area (not the header/sidebar/status bar), background
  `#000`, radius 12, 16:9 video surface centered. Top bar 48 with scrim gradient
  `rgba(0,0,0,0.6) → transparent`; bottom controls 56 with the reverse gradient.
- Seek bar: 4 px track `rgba(255,255,255,0.24)`, played `accent`, buffered
  `rgba(138,184,255,0.55)`, thumb 12 white. The buffered range is **only** drawn from real
  contiguous byte ranges reported by the engine. If the engine cannot report them, omit the
  buffered layer rather than animate a fake one.
- States: Preparing (spinner + “Preparing playback…”), Buffering (“Waiting for peers (7) ·
  12 s buffered”, auto-resume when 5 s contiguous), Playing, Paused, Ended, Error (danger
  banner with “Open folder” / “Retry”).
- “Play while downloading” entry points: row context menu Play, Files tab Play, details
  header Play, and double-click on a row whose torrent has exactly one playable file. The
  overlay opens immediately in Preparing; if no data arrives in 10 s, show Buffering with the
  peer count and a Cancel action.
- This blueprint assumes streaming support. Until the engine exposes sequential piece
  prioritization and buffered ranges, the overlay is reachable only for completed files (or
  from the existing external mpv action), and Play on an incomplete file is disabled with the
  tooltip from §5.4. Do not claim “streaming” in copy before that.
- Today the existing `player.rs` launches an external mpv window; the overlay is the target UI
  once mpv is embedded or controlled over IPC.

---

## 6. Table column set

Widths are at 1280 with the nav at 232; the table content width is ~1000. Name is the only
flex column. Cell padding is 12 px on each side, included in the width budget below.

| # | Column | Width | Default | Source today | Status |
|---|---|---|---|---|---|
| 1 | Name | flex, min 220 | yes | `TorrentSummary.name` | live |
| 2 | Size | 76 | yes | `total_bytes` | live; `—` before metadata |
| 3 | Progress | 128 (72 bar + 40 %) | yes | `progress` | live |
| 4 | Status | 96 | yes | `state` | live |
| 5 | Peers | 48 | yes | `peers` (connected only) | live |
| 6 | Down | 76 | yes | `download_rate` | live |
| 7 | Up | 76 | yes | `upload_rate` | live |
| 8 | ETA | 64 | yes | derived from rate + remaining | live; `—` at 0 rate |
| 9 | Remaining | 76 | no | derived from `total_bytes × (1-progress)` | live (derived) |
| 10 | Seeds | 52 | no | — | needs API (`seeds`, `seeds_total`) |
| 11 | Ratio | 56 | no | — | needs per-torrent up/down totals |
| 12 | Availability | 72 | no | — | needs piece availability |
| 13 | Category | 112 | no | — | needs engine category field |
| 14 | Tags | 112 | no | — | needs engine tag field |
| 15 | Added | 104 | no | — | needs `added_at` |
| 16 | Completed | 104 | no | — | needs `completed_at` |
| 17 | Save path | 180 | no | `TorrentDetail.download_dir` (detail only) | needs summary field |
| 18 | Tracker | 160 | no | — | needs tracker list |
| 19 | Private | 60 | no | `private` | live as lock icon + tooltip |
| 20 | Info hash | 120 | no | — | needs API |

Sum of the 8 default columns including 12 px padding each side:
`220 + 76 + 128 + 96 + 48 + 76 + 76 + 64 + (8×24) = 976`, leaving ~24 px slack at 1280.
At 1100 the table hides ETA (64) then Up (76); at 960 it hides Size (76) and scrolls.

**Seeds is deliberately absent from the default.** The engine reports a single connected
`peers` count, not a seed/leech split. Deriving seeds from peers would be a lie. Add the
column only when `TorrentSummary` carries `seeds` and `seeds_total`; until then, the details
General tab may show “Peers 14” and nothing else.

Column visibility menu: a 240 px context menu with checkboxes, “Resize to fit”, “Reset
columns”. Persist order/width/visibility in `ui-state.json`. The Name column cannot be hidden.

---

## 7. Interaction rules

### 7.1 Data honesty matrix (what the UI may render today)

| UI element | Backing field | Today | If missing |
|---|---|---|---|
| Name | `name` | yes | “Untitled torrent” |
| Size | `total_bytes` | yes | `—` before metadata, tooltip “Metadata not fetched” |
| Progress | `progress` | yes | — |
| State | `state` | yes | — |
| Peers | `peers` | yes | `0` is a real zero |
| Down / Up | `download_rate`, `upload_rate` | yes | `0 B/s` is real |
| ETA | derived | yes | `—` when rate is 0 |
| Seeds / leeches split | — | **no** | hide column |
| Ratio | — | **no** | hide; do not use session ratio per torrent |
| Availability | — | **no** | hide |
| Category / tags | — | **no** | hide tree groups |
| Added / completed dates | — | **no** | hide columns |
| Per-file progress | — | **no** | Files tab shows priority only |
| Peer list | — | **no** | hide Peers tab |
| Tracker list | — | **no** | hide Trackers tab |
| DHT node count | — | **no** | status bar shows “DHT on/off” only |
| Free disk space | OS stat (UI side) | P1 | hide the line until implemented |
| Session ratio | `Stats.bytes_uploaded / bytes_downloaded` | yes | `—` when zero downloaded; label “session” |

The unavailable state is consistent: a muted `—` in a cell, or a hidden control. Never `0`
for unknown; never a spinner that never resolves. An unavailable tab may show a centered
message with an `info` icon, 13/400 muted, and no table chrome.

`progress` means every **desired** piece, not every byte: a selective download can show 100%
with skipped files absent. Never recompute it from file sizes or show a second “downloaded”
number derived from it. The Files tab shows size and priority, not a per-file percentage.

### 7.2 Selection model

- Single click selects one torrent and opens/focuses details; `Ctrl+click` toggles; `Shift+click`
  selects a contiguous range from the anchor; `Ctrl+A` selects all visible (filtered) rows;
  `Esc` clears selection and closes the details pane.
- The anchor is the last clicked row; Shift+click extends from the anchor. Keyboard
  `Shift+Up/Down` extends, `Ctrl+Up/Down` moves focus without changing selection.
- Multi-selection shows “3 torrents selected” in the details pane header with aggregate
  actions (Pause, Resume, Remove); per-torrent tabs are hidden. Do not show the first
  torrent’s details as if it represented the selection.
- Selection survives filtering if the row remains visible; hidden selected rows are retained
  but not acted on by “select all visible”. The status bar shows “2 selected (1 hidden)” when
  relevant.
- Clicking empty space below rows clears selection. Clicking the drop strip does not clear.
- Row action menus operate on the clicked row; if it is not selected, select it first.

### 7.3 Mouse and double-click

| Gesture | Action |
|---|---|
| Single click | Select; show details |
| Double-click row | Open details (non-destructive default). If the torrent has exactly one playable file, open the player instead. |
| Double-click column divider | Auto-fit column to content |
| Right-click row | Context menu; select row if not selected |
| Middle-click row | No action (reserved) |
| Double-click empty area | No action |
| Click drop strip | Open Add sheet (file tab) |

Conflict resolved: qBittorrent's default double-click toggles start/stop. We choose the
non-destructive “open details/play” because a mis-double-click should not stop a transfer.
`Space` is the explicit pause/resume.

### 7.4 Play while downloading

The UI contract, independent of engine readiness:

1. A playable file exposes Play in the Files tab, the row context menu, and the details
   header when it is the only playable file.
2. Play opens the player overlay in `Preparing`; it does not wait for 100%.
3. Playback starts when the engine reports at least **5 s of contiguous data** from the
   playhead (or 2 MiB, whichever is smaller for audio). Until then, show `Buffering` with the
   connected peer count and a Cancel action.
4. Seeking moves the playhead; the engine reprioritizes pieces from the new position. The
   seek bar shows the buffered range in `rgba(138,184,255,0.55)` and the played range in
   `accent`; only real ranges.
5. If the buffer drains, pause and show `Buffering`; do not skip or fake playback.
6. Playback never changes file selection or priority silently. If the file is `Skip`, Play
   asks “Download this file to play it?” and sets it to `High` on confirm.
7. Closing the player does not stop the torrent; a “Playing” pill remains in the details
   header with a return-to-player action (P2).

Engine work required (not UI): a per-torrent sequential/stream mode, per-file contiguous
byte ranges, and `set_stream_position(file_index, offset)`. The existing `Picker` has
sequential ordering internally, but none of this is exposed through `flux-api`. Until it is,
Play is disabled for incomplete files with the tooltip from §5.4.

### 7.5 Keyboard shortcuts

Global (window):

| Shortcut | Action |
|---|---|
| `Ctrl+O` | Add .torrent file |
| `Ctrl+Shift+O` | Add magnet link |
| `Ctrl+V` | Paste magnet/URL from clipboard into Add sheet (explicit, never background monitoring) |
| `Ctrl+F` or `Ctrl+E` | Focus search |
| `Ctrl+,` or `Alt+O` | Settings |
| `Ctrl+A` | Select all visible |
| `Space` | Pause/resume selected |
| `Ctrl+S` | Start selected (qBittorrent-compatible) |
| `Ctrl+P` | Stop selected |
| `Delete` | Remove selected (confirm) |
| `Shift+Delete` | Remove + delete files (strong confirm) |
| `Enter` | Open details / activate focused row |
| `Ctrl+Enter` | Play the selected torrent’s primary playable file (if enabled) |
| `Ctrl+1..4` | Details tabs: General, Files, Peers, Trackers (expands details if collapsed) |
| `Ctrl+Shift+P` | Pause all |
| `Ctrl+Shift+S` | Resume all |
| `Esc` | Close modal/player, clear search, clear selection (in that order) |
| `F1` | About / shortcuts sheet |
| `Ctrl+Q` | Quit |

Table: `Up/Down` move, `Home/End` first/last, `PageUp/PageDown` by visible rows, `Shift+Up/Down`
extend, `Left/Right` collapse/expand a folder in Files, `Space` toggles pause on the focused
row, `Menu` key or `Shift+F10` opens the context menu.

Player: `Space` play/pause, `←/→` ±10 s, `Shift+←/→` ±1 s, `↑/↓` volume ±5%, `M` mute,
`F` fullscreen, `S` subtitles, `Esc` close overlay.

Shortcuts are ignored while a text field has focus, except `Esc`. `Ctrl+F` works from the
table and sidebar. Do not bind single-letter shortcuts globally.

### 7.6 Drag and drop

- Accept `.torrent` files anywhere in the client area. iced's window events report file
  drops (`FileDropped`), not arbitrary URL/text drops; the reliable magnet path is
  `Ctrl+V` paste. If a platform delivers a text drop, accept it, but do not claim URL drag.
- On drag-enter: show a full-content overlay, 2 px dashed `accent` border inset 12, scrim
  `rgba(0,0,0,0.40)`, icon `download` 32, “Drop to add 3 torrents” 18/600, sub “3 .torrent
  files” 12. The overlay does not block the drop.
- On drop: validate each item; add valid items, show one toast per batch (“3 added · 1
  duplicate”), and open the Add sheet only if metadata needs review. Multiple `.torrent`
  files add in one batch, not three modals. iced emits `FileDropped` once **per file**, so
  collect paths for 100 ms (or until the hover set matches the drop set) before adding;
  `FileHovered` is also per file, and `FilesHoveredLeft` resets the overlay.
- Reject folders, executables, and unknown extensions with a single error toast; do not
  silently ignore.
- Dragging a row out is not supported. Dragging a file out to Explorer is P2.
- The drop strip is a visual affordance; the whole window is the actual target.

### 7.7 Focus order and focus management

1. Header: search → Add split (main, then chevron) → overflow.
2. Sidebar: section headers are not focusable; items in visual order.
3. Toolbar: Pause → Resume → Remove → sort → Columns.
4. Table header: each sortable header.
5. Table rows (roving tabindex: only the focused row is in the tab order; arrows move).
6. Details: collapse/close → tabs → tab content controls.
7. Status bar: only interactive chips (DHT, limits) if any.

Tab moves forward, Shift+Tab backward; arrow keys move within a composite. Modals trap focus
and restore it to the trigger on close. The player traps focus while open. Focus is never
lost to `<body>` after a row is removed: move to the next row, then previous, then the table.

### 7.8 Long names, paths, and narrow windows

- Names: one line, trailing ellipsis, full text in tooltip after 500 ms; details header wraps
  to max 2 lines at 18/24, then ellipsis.
- Paths: middle ellipsis, preserving drive and final folder (`D:\…\OxideFlux`); tooltip full.
- Magnet URIs: one line, horizontal scroll in the input; on blur, middle ellipsis.
- Info hashes: 12 px mono, groups of 8, middle ellipsis at 24 chars; copy button.
- At <1100 the nav becomes a 64 px icon rail; labels move to tooltips; counts become badges.
- At <960 the table scrolls horizontally; the Name column keeps its 220 px minimum. Do not
  shrink text below 12 px or rows below 28 px to fit.
- At 125%/150% Windows scaling, all logical sizes scale; the min window becomes 1200×750
  physical at 125%. Verify no clipped labels at 150%.
- If the window is narrower than 960, clamp to min size rather than reflow into a mobile
  layout; this is a desktop app.

### 7.9 Persistence

Store in `ui-state.json` beside the engine state (or `%APPDATA%\OxideFlux\ui-state.json`):
window size/position/maximized, nav width, nav collapsed, details height/collapsed, column
order/widths/visibility, sort, filter, search history (last 5, P2), theme preference, density,
reduced motion, last selected torrent id. Write debounced 500 ms and on close. Version the
file (`version: 1`). Corrupt UI state falls back to defaults with a toast — unlike engine
state, it is not a hard error.

---

## 8. Accessibility

Targets: **WCAG 2.2 AA** for contrast and target size; AAA where cheap (focus appearance,
motion).

| Requirement | Rule |
|---|---|
| Body text contrast | ≥4.5:1 against its actual background; all pairs in §2 pass |
| Large text (≥18.66 px bold or ≥24 px) | ≥3:1 |
| Icons, progress, input borders, focus | ≥3:1 non-text |
| Disabled | exempt, but keep ≥2.5:1 for legibility |
| Minimum target | 24×24 (WCAG 2.5.8); our minimum is 32×32, rows 32 high |
| Focus indicator | 2 px `focus.ring`, ≥3:1 against adjacent colors, persistent until blur |
| Focus not obscured | sticky header never covers the focused row; scroll into view with 8 px margin |
| Keyboard | all actions reachable; no traps except modals/player (Esc exits) |
| Color independence | state icon + label; selected bar + weight; error icon + text |
| Motion | reduced-motion toggle; no parallax, no auto-playing decorative animation |
| Flashing | none; progress updates are value changes, not flashes |
| Text scaling | UI scales with OS DPI; no text below 11 px; layouts tolerate 150% |
| Screen reader | **not supported by iced 0.14** (no accessibility feature/accesskit). P0 ships keyboard + contrast; UIA/Narrator is a P2 research item requiring upstream work. Document this limitation in About. |

High contrast (P2): a theme variant with `#000` canvas, `#FFF` text, `#FFD400` focus/accent,
no soft tints, 1 px `#FFF` borders. Do not rely on OS high-contrast colors automatically;
iced custom themes bypass them.

---

## 9. iced 0.14 implementation plan

### 9.1 Crate and feature changes

```toml
iced = { version = "0.14", features = ["tokio", "svg", "advanced"] }
```

- `svg` is required for Lucide icons (`iced::widget::svg`).
- `advanced` is required for the custom table `Widget` (`iced::advanced`).
- `canvas` is not required; draw the table with renderer primitives.
- Add `serde` + `serde_json` for `ui-state.json` (already in the workspace via other crates;
  add explicit deps to `flux-app`).
- Keep `rfd` for native file/folder pickers; do not build a file browser in iced.

### 9.2 Module layout

```
crates/flux-app/src/
  app.rs                 // App state, Message, update, view composition
  main.rs
  player.rs              // existing external mpv; later MpvClient over IPC
  supervisor.rs
  ui/
    mod.rs
    tokens.rs            // Tokens, Colors, Spacing, Radius, TypeScale, Metrics
    theme.rs             // Theme::custom_with_fn + Extended from tokens
    style.rs             // button/container/input/progress/scrollable/pick_list styles
    icons.rs             // Icon enum, include_bytes!, icon()
    format.rs            // bytes, rate, duration, ratio, truncation
    table.rs             // VirtualTable custom Widget + ColumnSpec + Row
    components/
      mod.rs
      header.rs
      sidebar.rs
      toolbar.rs
      status_bar.rs
      empty.rs
      modal.rs
      toast.rs
      tabs.rs
      badge.rs
      progress.rs
      search.rs
    screens/
      library.rs
      add.rs
      details.rs
      settings.rs
      player.rs
```

`app.rs` stays the Elm loop. Screen modules return `Element<'_, Message>` and take `&App`
(or a focused view struct), never mutate. This keeps the existing actor/supervisor boundary
untouched.

### 9.3 Tokens and theme

```rust
// ui/tokens.rs
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colors {
    pub canvas: Color,
    pub surface: Color,
    pub raised: Color,
    pub overlay: Color,
    pub border: Color,
    pub border_strong: Color,
    pub border_input: Color,
    pub text: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub text_disabled: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_pressed: Color,
    pub accent_fill: Color,
    pub accent_fill_hover: Color,
    pub accent_fill_pressed: Color,
    pub accent_on: Color,
    pub accent_text: Color,
    pub accent_soft: Color,
    pub selection: Color,
    pub selection_bar: Color,
    pub focus: Color,
    pub success: Color,
    pub success_soft: Color,
    pub warning: Color,
    pub warning_soft: Color,
    pub danger: Color,
    pub danger_fill: Color,
    pub danger_fill_hover: Color,
    pub danger_fill_pressed: Color,
    pub danger_on: Color,
    pub danger_soft: Color,
    pub info: Color,
    pub info_soft: Color,
    pub progress_track: Color,
    pub scrollbar: Color,
    pub scrollbar_hover: Color,
}

#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub colors: Colors,
    pub dark: bool,
    pub space: [f32; 8],      // 0,4,8,12,16,24,32,48
    pub radius: [f32; 4],     // 4,8,12,999
    pub type_scale: TypeScale,
    pub metrics: Metrics,     // header 56, toolbar 44, row 32, status 28, nav 232
}

pub const DARK: Tokens = Tokens { /* hex values from §2.1 */ };
pub const LIGHT: Tokens = Tokens { /* §2.2 */ };

pub fn color(hex: u32) -> Color {
    Color::from_rgb8(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}
```

If `Color::from_rgb8` is not `const` in the pinned `iced_core`, build the constants with a
`const fn rgb` that returns a `Color` struct literal, or initialize once with `OnceLock`.
`Tokens` is `Copy`; closures capture it by value. This avoids a global and lets style
functions be pure:

```rust
pub fn primary(t: Tokens, status: button::Status) -> button::Style {
    let c = t.colors;
    let bg = match status {
        button::Status::Hovered => c.accent_fill_hover,
        button::Status::Pressed => c.accent_fill_pressed,
        _ => c.accent_fill,
    };
    let disabled = matches!(status, button::Status::Disabled);
    button::Style {
        background: Some(Background::Color(if disabled {
            Color { a: 0.4, ..c.accent_fill }
        } else { bg })),
        text_color: if disabled { c.text_disabled } else { c.accent_on },
        border: Border { radius: 999.0.into(), width: 2.0, color: Color::TRANSPARENT },
        shadow: Shadow::default(),
        ..button::Style::default()
    }
}
```

Focus is part of the style closure only when the widget has keyboard focus. iced's
`button::Status` has `Active`, `Hovered`, `Pressed`, `Disabled`; **it does not carry focus**.
The 2 px transparent border reserves the ring. To draw it, wrap the button in a `container`
whose style depends on an app-level `focused: Option<FocusId>` and a `FocusRing` wrapper, or
implement a small `Focusable` custom widget. Recommended: a `focus_ring(id, is_focused, child)`
container with 2 px padding and `background = focus` when focused, `Color::TRANSPARENT`
otherwise. This reserves the ring space and avoids layout shift. Do not pretend
`button::Status` has a focus variant.

Theme construction:

```rust
// ui/theme.rs
pub fn theme(t: Tokens) -> Theme {
    let base = iced::theme::Palette {
        background: t.colors.canvas,
        text: t.colors.text,
        primary: t.colors.accent_fill,
        success: t.colors.success,
        warning: t.colors.warning,
        danger: t.colors.danger,
    };
    Theme::custom_with_fn(
        if t.dark { "Oxide Flux Dark" } else { "Oxide Flux Light" },
        base,
        move |_| extended(t),
    )
}
```

`extended(t)` builds `palette::Extended` from our tokens so stock widgets (checkbox, radio,
toggler, pick_list menu, scrollable) use the same ramp instead of iced's generated tints. The
field names are public in `iced_core::theme::palette` 0.14:

```rust
fn extended(t: Tokens) -> iced::theme::palette::Extended {
    use iced::theme::palette::{Background, Danger, Extended, Pair, Primary, Secondary, Success, Warning};
    let c = t.colors;
    Extended {
        background: Background {
            base:  Pair { color: c.surface, text: c.text },
            weak:  Pair { color: c.canvas,  text: c.text },
            strong:Pair { color: c.raised,  text: c.text },
        },
        primary: Primary {
            base:  Pair { color: c.accent_fill, text: c.accent_on },
            weak:  Pair { color: c.accent_soft, text: c.accent_text },
            strong:Pair { color: c.accent_fill_pressed, text: c.accent_on },
        },
        secondary: Secondary {
            base:  Pair { color: c.overlay, text: c.text },
            weak:  Pair { color: c.surface, text: c.text },
            strong:Pair { color: c.border_strong, text: c.text },
        },
        success: Success { base: Pair { color: c.success, text: c.accent_on }, weak: Pair { color: c.success_soft, text: c.success }, strong: Pair { color: c.success, text: c.accent_on } },
        warning: Warning { base: Pair { color: c.warning, text: c.accent_on }, weak: Pair { color: c.warning_soft, text: c.warning }, strong: Pair { color: c.warning, text: c.accent_on } },
        danger:  Danger  { base: Pair { color: c.danger_fill, text: c.danger_on }, weak: Pair { color: c.danger_soft, text: c.danger }, strong: Pair { color: c.danger_fill_pressed, text: c.danger_on } },
        is_dark: t.dark,
    }
}
```

This is the single most important integration detail: without it, stock widgets drift toward
iced's purple/gray. Check the exact `Pair`/`Background` visibility against the pinned iced_core
version; if a field is private, fall back to `Extended::generate(base)` and style every stock
widget explicitly.

Registry:

```rust
iced::application(App::new, App::update, App::view)
    .title(|_, _| "Oxide Flux".to_owned())
    .theme(App::theme)                 // returns Tokens::current().theme()
    .window(window::Settings { ... })
    .font(INTER_REGULAR)
    .font(INTER_MEDIUM)
    .font(INTER_SEMIBOLD)
    .default_font(Font { family: Family::Name("Inter"), weight: Weight::Normal, ..Font::DEFAULT })
    .subscription(App::subscription)
    .run()
```

### 9.4 Font embedding

```rust
// ui/icons.rs or ui/fonts.rs
pub const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
pub const INTER_MEDIUM: &[u8]  = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

pub fn inter(weight: Weight) -> Font {
    Font { family: Family::Name("Inter"), weight, ..Font::DEFAULT }
}
```

- Ship static TTFs, not the variable font: iced/cosmic-text weight selection from a variable
  font is not reliable across platforms. Four files (400/500/600) are enough; do not ship
  Bold/Black.
- Include `assets/fonts/OFL.txt`. The OFL allows embedding and redistribution; keep the
  copyright notice.
- Fallback: if Inter fails to load, `Font::DEFAULT` resolves to the system sans (Segoe UI on
  Windows). Never embed Segoe UI (proprietary). For CJK/Arabic, cosmic-text's system fallback
  is used; if glyphs are missing, add a Noto Sans subset later, not a different UI face.
- Do not set font sizes below 11 or above 24 in chrome. The player title may use 24.

### 9.5 Icon embedding and tinting

```rust
#[derive(Debug, Clone, Copy)]
pub enum Icon { Add, Magnet, Search, Play, Pause, Trash, Folder, Settings, ChevronDown, More, /* ... */ }

const ICONS: &[(&str, &[u8])] = &[
    ("add", include_bytes!("../../assets/icons/add.svg")),
    ("magnet", include_bytes!("../../assets/icons/magnet.svg")),
    // ~40 entries, only icons actually used
];

pub fn svg_handle(icon: Icon) -> svg::Handle {
    svg::Handle::from_memory(ICONS[icon as usize].1)
}

pub fn icon<'a, M: 'a>(icon: Icon, size: f32, color: Color) -> Element<'a, M> {
    svg(svg_handle(icon))
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}
```

- `svg::Style { color: Some(..) }` is the supported tint filter; Lucide's `currentColor`
  resolves through it. Verify with one icon on day 1; if the renderer does not tint
  `currentColor`, preprocess the SVGs to `stroke="#FFFFFF"` and tint by multiplication. Do not
  ship a font icon set.
- Keep only used icons in `assets/icons/`; 40 × ~0.6 KB ≈ 24 KB. Include Lucide's `LICENSE`
  (ISC) in the same folder and in About → Licenses.
- Never recolor the app logo with the accent; the mark is neutral (`text.primary`) or a
  dedicated brand asset, not a Lucide icon.

### 9.6 Stock widgets vs custom

| UI piece | Approach |
|---|---|
| Window, title bar | Stock `window::Settings`, native decorations |
| Layout rows/columns | Stock `row!`, `column!`, `container`, `Space` |
| Scrollable nav/details/settings | Stock `scrollable` + custom style |
| Table/details split | Stock `pane_grid` (one column, two panes), custom separator style |
| Buttons | Stock `button` + token style; `focus_ring` wrapper for focus |
| Search/text input | Stock `text_input` with transparent background/border inside a styled container with leading icon |
| Dropdown | Stock `pick_list` + token style; stock menu overlay |
| Checkbox/radio/toggle | Stock widgets + token styles; size via custom style, not by scaling |
| Progress (details) | Stock `progress_bar` + token style |
| Progress (table) | Drawn inside `VirtualTable` (no widget per row) |
| Tabs | Custom `tabs` component (row of buttons + underline) |
| Modal | `stack![base, opaque(scrim), centered(card)]`; manual focus trap |
| Context menu | `float`/`stack` positioned overlay; custom menu list |
| Toast | `stack` + timed subscription; custom container |
| Tooltip | Stock `tooltip` P0; delayed custom wrapper P1 |
| Table | **Custom `VirtualTable` widget** (see §9.7) |
| Player video | OS child window (mpv `--wid`) or external mpv; not a normal iced widget |
| Drag/drop | `window::Event::FileDropped` / `FileHovered` subscription, full-window overlay |

**Focus traversal is not stock.** iced 0.14 does not implement automatic Tab traversal between
widgets (long-standing upstream issue #489). Build a `FocusChain` in `App`: an ordered list of
`widget::Id`s matching §7.7, `operation::focus_next` / `operation::unfocus` on `Tab` /
`Shift+Tab`, and `operation::is_focused` for the ring. Text inputs keep native Tab behavior
when they have focus (Tab inserts nothing but moves focus; use the chain). Without this,
keyboard navigation is a design document, not a product.

### 9.7 Table without a virtualization dependency

`iced::widget::table` exists in 0.14, but it constructs an `Element` for every row and every
cell on every view call. At 10,000 torrents that is 10,000 row trees per frame, and its style
hook is table-wide, not a full-row selected state. It has no windowing hook of its own.
`iced::widget::scrollable` does expose `on_scroll` in 0.14.0 (`Fn(Viewport) -> Message`), so a
windowed list is possible without a custom widget; the custom table is still the cleaner end
state. Do not use the stock table unguarded for the main library.

Three-stage plan:

**P0 — stock table, guarded.** Use `iced::widget::table` with the 8 default columns for the
first implementation. If `filtered_len > 500`, render only the first 500 and show a footer
“Showing 500 of 10,000 — refine your search”. This is not the final UX; it prevents a freeze
while the visual shell lands. Acceptance: 500 rows scroll at 60 fps.

**P1 — windowed list without a custom widget.** Build each row as a `button` containing a
`row!` of fixed-width cells (the current approach, cleaned up), inside a `scrollable` with
`.on_scroll(|viewport| Message::TableScrolled(viewport.absolute_offset().y))`. Keep a window
of `visible_rows + 20` rows around the offset, with top/bottom `Space` spacers sized from the
offset, and request only that window. This is real windowing with no third-party crate and no
custom `Widget`; it is acceptable for 2,000–5,000 rows. It costs one message per scroll frame
and a full view rebuild of ~40 rows, so it can stutter on very fast wheel/trackpad scroll;
use it only as a stepping stone. Note: 0.14.0's callback takes `Viewport`; later 0.14.x
patches change it to `Scroll`/`Action`. Pin the exact minor and adapt the closure.

**P1/P2 — custom `VirtualTable` (recommended).** One widget draws only visible rows, owns
scrolling, selection, hover, keyboard, and column resizing. This is the final architecture.

```rust
pub struct ColumnSpec {
    pub id: ColumnId,
    pub title: &'static str,
    pub width: Length,       // Fixed(px) or Fill
    pub min_width: f32,
    pub align: Alignment,    // Start / End
}

pub struct VirtualTable<'a, Message> {
    columns: &'a [ColumnSpec],
    rows: &'a [Row],          // display-ready, sorted/filtered indices applied
    row_height: f32,
    selected: &'a HashSet<TorrentId>,
    focused: Option<TorrentId>,
    sort: Sort,
    on_press: Box<dyn Fn(TorrentId, Modifiers) -> Message + 'a>,
    on_activate: Box<dyn Fn(TorrentId) -> Message + 'a>,
    on_context: Box<dyn Fn(TorrentId, Point) -> Message + 'a>,
    on_resize: Box<dyn Fn(ColumnId, f32) -> Message + 'a>,
}
```

`Widget::layout` returns a node of `width = available`, `height = rows.len() * row_height`
(capped at `f32::MAX`), so the parent scrollable has a real scrollbar. `Widget::draw`:

1. Compute `visible = viewport.intersection(layout.bounds())`.
2. `first = ((visible.y - bounds.y) / row_h).floor().max(0) as usize`.
3. `last = ((visible.y + visible.height - bounds.y) / row_h).ceil() as usize`, clamp.
4. Fill the visible background, then for each row in `first..last`, fill the row quad
   (`selection`, `hover`, or `surface`), the 2 px selection bar, and draw the cell text with
   `renderer.fill_text` / prebuilt `Text` layouts. Format numbers only for visible rows.
5. Draw the focused row's 2 px ring and the column separator lines.

`Widget::update` handles `Mouse::ButtonPressed(Left)`, `CursorMoved`, `MouseWheel` (delegate
to the parent scrollable), and modifier state (captured from a `keyboard::Event::ModifiersChanged`
subscription stored in `App`). `mouse_interaction` returns `Pointer` over a row,
`ResizingHorizontally` over a header divider. `operate` supports `scroll_to` and focus
requests. Header is a separate strip above the scrollable, sharing `ColumnSpec` widths; it
does not scroll.

Why not `Table`: no per-row full-width style hook, every row is built, and there is no 10k
path. Why not a third-party table crate: adds an API/dep for a widget that is ~500 lines and
tightly coupled to our columns; iced 0.14's `advanced` API is stable enough.

### 9.8 Performance for 10,000 torrents

| Concern | Rule |
|---|---|
| Snapshot rate | 4 Hz for the list (250 ms), 1 Hz for status aggregates. The engine may push at 10 Hz; coalesce in `drain_events` and render the latest. |
| Detail fetch | Fetch `GetDetail` on selection change and at most 1 Hz for the selected torrent. The current “every snapshot” call is 10 RPCs/s and must go. |
| Names | Convert `String` to `Arc<str>` once per snapshot; share across sorted/filtered views. Drop the original snapshot. |
| Sorting | Sort a `Vec<u32>` of indices with `sort_unstable_by`; 10k ≈ 1 ms. Re-sort only when snapshot or sort changes, not per frame. |
| Filtering | One pass into a reusable `Vec<u32>`; compare lowercase names only when the query changes. Cache the lowercase name in `Row` (`Arc<str>` + `String` lower) only if profiling shows it matters. |
| Formatting | Format bytes/rates/ETA for **visible rows only**, into a stack buffer or `format!` (40 rows/frame is fine). Never preformat 10k strings per snapshot. |
| Text layout | cosmic-text caches layouts; visible-only text keeps the cache small. Reuse `Text` structs per column. |
| Draw calls | ~40 rows × (2 quads + 8 text runs) ≈ 400 calls; no shadows, no clips per cell. The table is one clip rect. |
| Memory | `Row` ≈ 80 B + `Arc<str>` name ≈ 2–4 MB for 10k; indices ≈ 40 KB. |
| Scroll | Fixed row height → O(1) visible range; no layout of offscreen rows. |
| Progress | Interpolate only visible bars; 30 fps max; do not trigger a full view rebuild for a 0.1% change. |
| Status bar | Sum rates from the snapshot, not per row per frame; 1 Hz is enough. |
| Target | 60 fps scroll at 10k rows, <3 ms CPU/frame at 1280×800, <5% of one core idle, UI RSS <80 MB excluding the engine. |

Also: avoid `Lazy` for the table (it still diffs the whole tree); avoid `keyed_column` for
10k rows; avoid `Shadow` on rows; avoid rebuilding the sidebar counts more than once per
snapshot.

### 9.9 Migration from the current `app.rs`

| Current | Change |
|---|---|
| `Theme::Dark` | `Tokens::DARK.theme()` + `Theme::custom_with_fn` |
| `button("Pause")` default purple | `button(...).style(move |_, s| style::secondary(t, s))` |
| `button::primary` / `button::secondary` / `button::text` sidebar/table | Token styles; no built-in catalog styles anywhere |
| `button("Add .torrent")` text-only | Primary split button with `plus` icon; Add magnet in the chevron menu |
| `magnet_open` inline row | Add sheet modal (file/magnet segmented) |
| `table_header` + `table` column | `VirtualTable` with `ColumnSpec`; sticky header outside scroll |
| `button(line)` table rows | Custom row draw; modifiers from `keyboard::Event::ModifiersChanged` |
| `details()` fixed 160 px list | `pane_grid` + tabs + resizable details; Files tree |
| `text("Nothing here yet.")` | `empty::library()` and `empty::no_results()` |
| `error` row with Dismiss | Toast queue + inline engine banner |
| `Filter` enum All/Downloading/Completed/Paused | Add Seeding, Checking, Error; counts computed once |
| status line string | Structured status bar with icons, DHT on/off, session ratio |
| 100 ms `Pump` + detail every tick | 250 ms snapshot; detail on change + 1 Hz |
| default title “Flux App - Iced” | `.title(|_, _| "Oxide Flux")`, window icon |
| `progress_cell` stock bar | Drawn bar + percent in row |
| `pick_list` priorities inline in details | Files tab dropdown, 3 options, 24 px high |

Do this migration in P0 behind the existing `Message` enum where possible; keep the
`supervisor`/`Engine` boundary untouched.

### 9.10 Fonts, assets, and licenses in-repo

```
crates/flux-app/assets/
  fonts/Inter-Regular.ttf
  fonts/Inter-Medium.ttf
  fonts/Inter-SemiBold.ttf
  fonts/OFL.txt
  icons/*.svg            // only the ~40 used
  icons/LICENSE          // Lucide ISC
  app-icon.rgba          // 256×256, generated from the mark
```

`include_bytes!` paths are relative to the source file; keep `assets/` inside the crate so
`cargo package` includes it. Add `assets/**` to the crate's `include` if publishing. The About
dialog lists Lucide (ISC), Inter (OFL 1.1), and iced (MIT) with license text links.

---

## 10. Prioritized backlog

### P0 — the shell and the truth (must ship first)

| # | Item | Acceptance criteria |
|---|---|---|
| P0.1 | Token layer + dark/light themes | `ui/tokens.rs` + `ui/theme.rs`; all 6 iced palette roles mapped; `Theme::custom_with_fn`; no `Theme::Dark` or built-in button styles outside `ui/`; CI grep gate passes. |
| P0.2 | Native chrome + header | Title “Oxide Flux”, 1280×800 default, 960×600 min, icon; header 56 with wordmark, version, search, Add split, overflow; no purple anywhere. |
| P0.3 | Sidebar + counts | 7 statuses with counts, selected state, Save-to block, 232 width, keyboard navigable; zero-count items still visible; no empty Categories/Tags. |
| P0.4 | Table shell | 32 px rows, sticky 32 px header, 8 default columns, trailing ellipsis, full-row hover/selection, sort cycle, keyboard nav, `—` for unknown; 500 rendered rows scroll at 60 fps (10k guarded by P0.11). |
| P0.5 | Buttons/inputs/badges | Primary/secondary/ghost/danger/icon with all six states; 32 px inputs, search with leading icon, checkbox/radio/toggle, state/private/ratio badges; focus ring visible via keyboard. |
| P0.6 | Empty state + drop zone | Empty screen at 0 torrents with dashed 180 px zone; drag-over state; drop adds; no-results variant; screenshot at 1280×800. |
| P0.7 | Add sheet | File + magnet tabs; parse/fetch states; file tree with Skip/Normal/High; save path; Start toggle; duplicate warning; primary disabled with 0 files; Esc/Enter. |
| P0.8 | Details pane | `pane_grid` split; General + Files only (API-backed); collapse to 40; resizable 160–400; no Peers/Trackers tabs until data exists; no per-file progress. |
| P0.9 | Status bar + errors | Counts, DHT on/off, ↓/↑, session ratio (or `—`), selection; engine-stopped banner; toast queue; no fake DHT nodes. |
| P0.10 | Interaction + a11y pass | Selection model, shortcuts (add/pause/delete/search/tabs/play), context menu, focus order, 2 px rings, contrast tests, reduced-motion toggle. |
| P0.11 | Performance guard | Snapshot 4 Hz, detail 1 Hz, no per-frame 10k formatting; 10k synthetic rows do not freeze; stock table capped at 500 until the virtual table lands. |

P0 exit criteria: `cargo test --workspace` green; `cargo clippy --workspace --all-targets -- -D warnings`
green; manual QA at 1280×800 and 150% scaling; no default purple in a screenshot; all
unavailable data shown as `—`, not `0`.

### P1 — density and streaming readiness

| # | Item | Acceptance criteria |
|---|---|---|
| P1.1 | `VirtualTable` | 10k rows, 60 fps scroll, sticky header, column resize/reorder (reorder P2), full-row hover/selection, keyboard nav; <3 ms CPU/frame. |
| P1.2 | Column chooser + persistence | Menu with visibility/reset; widths/order persisted; Name cannot hide; restore after restart. |
| P1.3 | Settings dialog | Left nav + grouped rows; live theme/density/reduced motion/hide-zero-speeds; restart-required badges; save/cancel/dirty guard; no dead settings. |
| P1.4 | API-gated columns/tabs | Seeds, ratio, availability, category, tags, added/completed, peers/trackers tabs land only when the DTO fields exist; each has an empty/unavailable state and a test. |
| P1.5 | Play while downloading | Sequential toggle + buffered ranges + `set_stream_position`; player overlay with Preparing/Buffering/Playing; Play disabled with reason until then. |
| P1.6 | mpv IPC controls | Named-pipe client; play/pause/seek/volume/stop from the overlay; external window fallback; no fake state. |
| P1.7 | Search + filters | 150 ms debounce; name/category/tag/tracker; result count; clear; `Ctrl+F`; no-results state. |
| P1.8 | Drag/drop batch | Multi-file `.torrent` drops, paste magnet, overlay count, duplicate reporting, rejection toast. |
| P1.9 | Light theme + density | Both themes pass the contrast table; compact 28 px; screenshots; no theme-specific layout shifts. |
| P1.10 | Delayed tooltip + context menu polish | 500 ms delay, keyboard open, edge flipping, submenu delay, danger styling. |

### P2 — polish and platform depth

| # | Item | Acceptance criteria |
|---|---|---|
| P2.1 | Embedded mpv | `--wid` child window inside the content pane; resize tracking; fallback external; no black rectangle on close. Requires an iced raw-window-handle spike. |
| P2.2 | Custom title bar | Only if Snap Layouts, resize borders, high contrast, and double-click-maximize are preserved; otherwise keep native. |
| P2.3 | Accessibility tree | UIA/Narrator via accesskit or upstream iced; labels/roles for table, buttons, dialogs; keyboard remains the baseline. |
| P2.4 | High-contrast theme | Pure black/white/yellow; 1 px white borders; no soft tints; passes 7:1. |
| P2.5 | Motion polish | Pane/modal/toast transitions; reduced-motion honored; no layout jank. |
| P2.6 | Continue watching | Top strip in Library with resume progress and Play; only for files with playback position; dismissible; not a content catalog. |
| P2.7 | Pieces tab / speed graph | Only with real piece maps/history; otherwise absent. |
| P2.8 | Multi-select batch bar | Floating action bar with counts, pause/resume/remove, “3 of 5 selected” semantics. |
| P2.9 | Tray + close-to-tray | Native tray (platform crate; not an iced widget); close behavior setting; restore; no fake “background engine” if the process exits. |

---

## 11. Open questions / dependencies

1. **Streaming API.** `flux-api` has no sequential mode, per-file byte counts, buffered
   ranges, peer list, tracker list, categories, tags, added/completed timestamps, or DHT node
   count. The design is ready for them; the UI must not pre-empt them. P1.4/P1.5 are blocked
   on DTO changes.
2. **mpv embedding.** iced 0.14 exposes the raw window handle only through `window::run`'s
   `&dyn Window`, not through the Elm `view`. The `--wid` child-window path is a P2 spike that
   must create the mpv process from a window task, not from `view`. The fallback is an external
   mpv window with IPC controls. Do not promise “embedded” in P0 copy.
3. **Accessibility.** iced 0.14 has no accessibility feature; Narrator/UIA is P2 and may need
   upstream changes.
4. **Column “Seeds”.** The brief asks for seeds and peers; the engine reports only connected
   peers. Ship Peers now, Seeds when the DTO splits them. Do not approximate.
5. **FDM dark palette.** No official dark screenshot was available; the dark hexes are
   derived. Validate against real FDM 6.36 dark mode before freezing tokens.
6. **UI state location.** `%APPDATA%\OxideFlux\ui-state.json` vs next to `.fluxd-state.json`.
   Recommend `%APPDATA%` on Windows, XDG config on Linux; keep engine state where it is.
