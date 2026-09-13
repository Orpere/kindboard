# Theme + responsive contract (ADR-0016)

Implementer-facing contract for the frontend-engineer. This is the authoritative
list of signatures, renames, and responsive changes. If a decision is not here,
ask the architect — do not re-derive.

See [ADR-0016](adrs/ADR-0016.md) for rationale.

---

## 1. Theme module — exact public API

`crates/kindboard-app/src/theme.rs` is rewritten to expose:

```rust
//! Visual identity: runtime-selectable themes (dark/light/high-contrast).

use eframe::egui::{self, Color32, CornerRadius, Margin, Style, Visuals};
use std::sync::atomic::{AtomicU8, Ordering};

/// One of the three selectable themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeId {
    Dark,
    Light,
    HighContrast,
}

impl ThemeId {
    /// All themes in UI order.
    pub const ALL: [ThemeId; 3] = [ThemeId::Dark, ThemeId::Light, ThemeId::HighContrast];
    /// Default theme (also the fallback for missing/invalid settings).
    pub const DEFAULT: ThemeId = ThemeId::Dark;
    /// Stable persistence id: "dark" | "light" | "high-contrast".
    pub fn id(self) -> &'static str;
    /// UI label: "Dark" | "Light" | "High Contrast".
    pub fn label(self) -> &'static str;
    /// Parse a persisted id (case-insensitive); `None` for unknown ids.
    pub fn from_id(id: &str) -> Option<ThemeId>;
    /// `true` only for [`ThemeId::Dark`].
    pub fn is_dark(self) -> bool;
    /// The palette for this theme.
    pub fn palette(self) -> &'static Palette;
}

/// A full visual palette. `Copy`, field-for-field `const`-constructible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    // --- brand/status (all themes) ---
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub green: Color32,
    pub amber: Color32,
    pub red: Color32,
    pub grey: Color32,
    pub succeeded: Color32,   // replaces the inline 0x4f9dc9 (util.rs:48, diagram.rs:86)
    // --- surfaces/structure ---
    pub bg: Color32,
    pub bg_raised: Color32,
    pub stroke: Color32,
    pub canvas_bg: Color32,
    pub text_dim: Color32,
    pub extreme_bg: Color32,  // replaces the inline 0x0a0d11 in apply()
    // --- diagram-only colors (promoted from hardcoded values) ---
    pub node_text: Color32,   // diagram node label (was 0xd6e4ee, diagram.rs:389)
    pub tooltip_bg: Color32,  // diagram tooltip fill (was from_black_alpha(220), diagram.rs:306)
    pub tooltip_text: Color32,// diagram tooltip text (was WHITE, diagram.rs:299/310)
    pub service_fill: Color32,// diagram service fill (was rgba 0x5b8db8 @ 40/255, diagram.rs:356)
    pub ingress_fill: Color32,// diagram ingress fill (was rgba 0x8d7ab8 @ 40/255, diagram.rs:361)
    // --- per-theme dim behavior ---
    dim_lighten: f32,         // 0.0 → darken path (Dark); >0 → lighten toward white
}

impl Palette {
    /// Background "dim" variant of a status color for pills/badges/error strips.
    ///
    /// - Dark: `c/5` (byte-wise) — exact current behavior.
    /// - Light: lighten toward white, `c + (255 - c) * 0.80` (round).
    /// - HighContrast: `c/2` (byte-wise) — half-brightness hue, visible on black.
    #[must_use]
    pub fn dim(&self, color: Color32) -> Color32;
}

// ---- registry + accessors ----

/// The active palette (per-frame hot path; no lock, no allocation).
pub fn pal() -> &'static Palette;

/// The active theme id.
pub fn current() -> ThemeId;

/// Set the active palette index without touching the egui context (used at
/// startup so the first `apply` uses the persisted theme).
pub fn set_index(id: ThemeId);

/// Apply the active theme's `Visuals` + `Style` to `ctx`.
/// Signature unchanged from today (called once in `KindboardApp::new`).
pub fn apply(ctx: &egui::Context);

/// Swap the active theme and re-apply visuals/style (theme-switch UI).
pub fn set_theme(ctx: &egui::Context, id: ThemeId);

/// Read the persisted theme from `DataDir::standard()/settings.json`.
/// Falls back to `ThemeId::Dark` on any failure (missing dir, unparseable,
/// unknown id). Used once by `main` before the window exists.
pub fn load_persisted_theme() -> ThemeId;
```

Internal (private) implementation notes:

- `static PALETTES: [Palette; 3] = [dark(), light(), high_contrast()];`
  (each `fn` is a `const fn`), and `static CURRENT: AtomicU8 = AtomicU8::new(0);`.
- `pal()` = `&PALETTES[CURRENT.load(Ordering::Relaxed)]` (index is always 0..=2
  because `set_index`/`set_theme` only ever store `id as u8` from the enum).
- `apply(ctx)` = `apply_theme(ctx, current())`; `apply_theme` starts from
  `Visuals::dark()` (Dark) or `Visuals::light()` (Light/HighContrast), overrides
  the fields currently set in `apply` (`panel_fill`, `window_fill`,
  `window_stroke`, `selection.*`, `hyperlink_color`, `widgets.*` bg/strokes,
  `extreme_bg_color`), then applies the **same `Style`** as today for every theme.
- `dim` uses the byte-wise `/5` or `/2` (integer division) for Dark/HighContrast
  and the `c + (255-c)*k` round-to-nearest blend for Light. `dim_lighten` is
  `0.0` (Dark), `0.80` (Light), `0.0`+`/2`-flag handled via a small private
  `DimMode` if preferred — the *observable* behavior above is the contract.

### Palette reference values (Dark = current, Light/HighContrast = initial, QA-verify contrast)

| field | Dark | Light | HighContrast |
|---|---|---|---|
| accent | `0x3da5d9` | `0x1b7fb2` | `0x1f9bd6` |
| accent_hover | `0x3d86ad` | `0x186c97` | `0x3da5d9` |
| on_accent | `0x060d12` | `0xffffff` | `0x000000` |
| green | `0x4dc064` | `0x1f8a3d` | `0x00c853` |
| amber | `0xf0a52e` | `0xb36b00` | `0xff9800` |
| red | `0xe5534b` | `0xc0392b` | `0xff3b30` |
| grey | `0x8a96a3` | `0x6b7683` | `0x90a4ae` |
| succeeded | `0x4f9dc9` | `0x2b7ea3` | `0x4fc3f7` |
| bg | `0x12161c` | `0xf5f7fa` | `0x000000` |
| bg_raised | `0x1a2028` | `0xffffff` | `0x121212` |
| stroke | `0x2c3642` | `0xd0d7df` | `0xffffff` |
| canvas_bg | `0x0e1217` | `0xeef1f5` | `0x000000` |
| text_dim | `0x9aa6b4` | `0x5a6672` | `0xcfd8dc` |
| extreme_bg | `0x0a0d11` | `0xe6eaf0` | `0x000000` |
| node_text | `0xd6e4ee` | `0x2b333b` | `0xffffff` |
| tooltip_bg | `black_alpha(220)` | `black_alpha(230)` | `black` (opaque) |
| tooltip_text | `WHITE` | `WHITE` | `WHITE` |
| service_fill | `rgba(0x5b8db8, 40)` | `rgba(0x5b8db8, 36)` | `rgba(0x5b8db8, 60)` |
| ingress_fill | `rgba(0x8d7ab8, 40)` | `rgba(0x8d7ab8, 36)` | `rgba(0x8d7ab8, 60)` |
| dim_lighten | 0.0 (darken `/5`) | 0.80 (lighten) | 0.0 (`/2` via HighContrast) |

---

## 2. `Settings` + bus + wiring changes

### 2.1 core `Settings` (`crates/kindboard-core/src/state/mod.rs`)

Add one field (update the manual `impl Default` too):

```rust
/// Active theme id ("dark" | "light" | "high-contrast"). `None` = default (dark).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub theme: Option<String>,
```

- Old `settings.json` without `theme` → `theme = None` (Dark). No migration.
- Unknown keys keep flowing into `#[serde(flatten)] other` (unchanged).

### 2.2 bus (`crates/kindboard-app/src/bus.rs`)

Add one command variant (no new event is required):

```rust
/// Persist the selected theme id.
SetTheme {
    /// `"dark"` | `"light"` | `"high-contrast"`.
    id: String,
},
```

### 2.3 worker (`crates/kindboard-app/src/worker.rs`)

Handle the new variant in the `tokio::select!` dispatch, spawned like the
other commands (never blocks the UI thread; `DataDir` is already in `env`):

```rust
CoreCommand::SetTheme { id } => {
    let tx = event_tx.clone();
    let worker_env = env.clone();
    tasks.spawn(async move {
        let result = worker_env.data_dir.load_settings()
            .map(|mut s| { s.theme = Some(id); s })
            .and_then(|s| worker_env.data_dir.save_settings(&s));
        if let Err(err) = result {
            emit(&tx, CoreEvent::Notice { message: format!("could not save theme: {err}") });
        }
    });
}
```

(`emit`/`emit_important` already exist in worker.rs.)

### 2.4 startup (`crates/kindboard-app/src/main.rs`)

```rust
let initial_theme = kindboard_app::theme::load_persisted_theme();
let app = KindboardApp::new(cc, cmd_tx, event_rx, initial_theme);
```

### 2.5 `KindboardApp::new` (app.rs)

Add the parameter and apply it before the existing `theme::apply`:

```rust
pub fn new(
    cc: &eframe::CreationContext<'_>,
    cmd: Sender<CoreCommand>,
    events: Receiver<CoreEvent>,
    initial_theme: ThemeId,
) -> Self {
    theme::set_index(initial_theme);
    theme::apply(&cc.egui_ctx);
    // … (rest unchanged)
}
```

### 2.6 theme switcher UI (app.rs `render_top_bar`)

In the right-to-left cluster, before `About`, add:

```rust
let mut theme = theme::current();
egui::ComboBox::from_id_salt("theme-picker")
    .selected_text(theme.label())
    .show_ui(ui, |ui| {
        for id in ThemeId::ALL {
            ui.selectable_value(&mut theme, id, id.label());
        }
    });
if theme != theme::current() {
    theme::set_theme(ui.ctx(), theme);
    self.issue(CoreCommand::SetTheme { id: theme.id().to_string() });
}
```

---

## 3. Mechanical rename list (exact)

Every symbol currently referenced as `theme::X` must become `theme::pal().x`.
Total **135 references** across 133 lines / 10 files. `sed`-safe ordering:
rename `dim` last or use word-boundary edits (never match `theme::dim` inside
`theme::dim(` blindly — there is no other `dim*` symbol, so a simple ordered
replace is fine).

| # | from | to | refs |
|---|---|---|---|
| 1 | `theme::TEXT_DIM` | `theme::pal().text_dim` | 43 |
| 2 | `theme::RED` | `theme::pal().red` | 32 |
| 3 | `theme::AMBER` | `theme::pal().amber` | 15 |
| 4 | `theme::GREEN` | `theme::pal().green` | 14 |
| 5 | `theme::ACCENT` | `theme::pal().accent` | 9 |
| 6 | `theme::GREY` | `theme::pal().grey` | 8 |
| 7 | `theme::dim(` | `theme::pal().dim(` | 6 |
| 8 | `theme::STROKE` | `theme::pal().stroke` | 3 |
| 9 | `theme::BG_RAISED` | `theme::pal().bg_raised` | 2 |
| 10 | `theme::ON_ACCENT` | `theme::pal().on_accent` | 1 |
| 11 | `theme::BG` | `theme::pal().bg` | 1 |
| 12 | `theme::apply(` | `theme::apply(` (unchanged) | 1 |

Notes:

- **`theme::ACCENT.r()/.g()/.b()`** (diagram.rs:345-347) are 3 of the 9 `ACCENT`
  refs. After the rename they become `theme::pal().accent.r()` etc. — still
  valid, no extra work (`pal().accent` is a `Color32`).
- **Nested `dim`** appears 3×: `theme::dim(theme::RED)` at overview.rs:109,
  cluster.rs:578, app.rs:618 → `theme::pal().dim(theme::pal().red)` (rules 7+2).
- **`card_frame`/`canvas_frame`** had zero call sites and were **removed**
  during implementation (cleanup directive) — see §7 amendments.

### Per-file reference counts (for review)

| file | refs |
|---|---|
| views/cluster.rs | 35 |
| views/diagram.rs | 29 |
| views/overview.rs | 28 |
| views/deps.rs | 12 |
| app.rs | 7 |
| util.rs | 7 |
| views/ops.rs | 6 |
| views/wizard.rs | 5 |
| views/about.rs | 3 |
| icons.rs | 1 |

---

## 4. Responsive strategy per view (R3)

New minimum window size: **640×480** (main.rs `with_min_inner_size`).

Justification: 640 is the smallest width where the cluster tab's two side
panels (left 150 + right 200 at their mins) still leave a usable central
diagram (~250px at min zoom 0.2), and where the overview deps panel (min 240)
leaves ~400px for a single-column card grid. 480 is the smallest height where
the top bar + tab bar + a scrollable central panel remain legible. Every
mechanism below also works at 600×400 if product later wants to go lower —
640×480 is chosen as the UX floor, not a technical limit.

### 4.1 Wizard modal (wizard.rs:117-126, 138, 157, 163, 253)

- `ui.set_width(520.0)` → `ui.set_width(ui.available_width().min(520.0))`.
- Wrap the form body in `egui::ScrollArea::vertical().id_salt("wizard-scroll")
  .max_height(ui.available_height().max(120.0))` so the 10-field form scrolls
  on short screens (the heading + Create/Cancel buttons stay outside the
  scroll area).
- Every `TextEdit::singleline(..).desired_width(N)` inside the modal →
  `.desired_width(f32::INFINITY)` (fill the clamped modal width; N∈{360,200,160,220}).

### 4.2 Destroy/scale confirm modal (overview.rs:195-240, 341)

- `ui.set_width(400.0)` (destroy) → `ui.set_width(ui.available_width().min(400.0))`.
- `TextEdit … .desired_width(360.0)` → `.desired_width(f32::INFINITY)`.
- `ui.set_width(330.0)` (scale body) → `ui.set_width(ui.available_width().min(330.0))`.

### 4.3 Tab-confirm modal (cluster.rs:967-979)

- `ui.set_width(430.0)` → `ui.set_width(ui.available_width().min(430.0))`.
- `TextEdit … .desired_width(390.0)` → `.desired_width(f32::INFINITY)`.

### 4.4 Overview cards grid (overview.rs:177-183, 335-341)

- `render_card` is already inside `horizontal_wrapped` + vertical `ScrollArea`.
  Change the card body `ui.set_width(330.0)` →
  `ui.set_width(ui.available_width().min(330.0))`. Cards then shrink to the
  panel and wrap to one column at small widths (no horizontal overflow).

### 4.5 Overview deps right panel (app.rs:701-704)

- `.default_size(380.0)` → `.default_size(320.0).min_size(240.0).max_size(480.0)`
  (resizable `egui::Panel` supports `min_size`/`max_size` in egui 0.36).
- The panel content is already in a `ScrollArea::vertical().auto_shrink([false,false])`;
  additionally ensure the per-tool rows and the inline install log wrap: use
  `ui.horizontal_wrapped` for the `Dependencies` header row (deps.rs:193) and
  keep tool rows on `ui.horizontal` (they're narrow enough at 240), but wrap
  long monospace install-log lines — set `ui.style_mut().wrap_mode =
  egui::TextWrapMode::Wrap` in the install-log loop (deps.rs ~319) or render
  the log inside `ScrollArea::both()`.

### 4.6 Cluster tab panels (cluster.rs:381-476)

- Nodes left panel `default_size(190.0)` → `.default_size(180.0).min_size(150.0).max_size(260.0)`.
- Detail right panel `default_size(290.0)` → `.default_size(280.0).min_size(200.0).max_size(360.0)`.
- Logs bottom panel `default_size(190.0)` → `.default_size(180.0).min_size(120.0).max_size(320.0)`.
- Central diagram already fit-to-view/pan/zoom (`DiagramState::fit_next`); no
  change (the canvas is a `CentralPanel`, so it absorbs whatever the side
  panels leave).

### 4.7 Diagram toolbar (cluster.rs:481-573)

- The toolbar (`Refresh now`, `Auto-refresh`, interval combo, `Collapse all`,
  `Expand all`, `Fit to view`, spinner) is wide. Change `ui.horizontal(|ui| {…})`
  → `ui.horizontal_wrapped(|ui| {…})` so it wraps at 640 instead of clipping.

### 4.8 Top bar + tab bar (app.rs:570-688)

- Top bar (brand + title + version + bus-warning + About + theme picker):
  `ui.horizontal` → `ui.horizontal_wrapped` (the bus-warning string wraps
  instead of clipping).
- Tab bar (Overview + N closable cluster tabs): wrap the tabs in
  `egui::ScrollArea::horizontal().id_salt("tabs")
  .auto_shrink([false, true]).max_height(40.0)` so many tabs scroll
  horizontally instead of overflowing (each tab's inner `ui.horizontal` stays).

### 4.9 About window (about.rs:84-88)

- No functional change required (420-wide, short, wraps; egui clamps windows to
  the screen). For extra safety set `.default_width(400.0.min(..))` — optional.
  Verify at 640×480 via screenshot.

### 4.10 Ops window (ops.rs:256-303)

- The output tail is `ScrollArea::vertical().stick_to_bottom(true).max_height(220.0)`
  with monospace lines that can be wider than the window. Change to
  `ScrollArea::both()` (keep `.stick_to_bottom(true)` and `.max_height(220.0)`)
  so long lines scroll horizontally instead of overflowing; optionally wrap via
  `ui.style_mut().wrap_mode = egui::TextWrapMode::Wrap`.
- `egui::Window … .resizable(true).default_width(560.0)` is fine (560 < 640);
  leave as-is.

---

## 5. Risks / unknowns for the implementer

1. **Brand colors (icons.rs `tool_color`, util.rs:70-80) stay fixed.** They are
   tool *logo* identities (Docker blue, Kubernetes blue, …), not theme colors.
   Do **not** theme them. The monogram `tool_icon` fill (alpha 46) remains
   legible on light and dark. Only verify contrast in QA.
2. **Diagram `Succeeded` blue is duplicated** (util.rs:48 `pod_phase_color` and
   diagram.rs:86 `NodeStatus::color` both hardcode `0x4f9dc9`). Replace both
   with `theme::pal().succeeded` so the status language stays consistent across
   themes (this is a `theme::`-adjacent change beyond the pure rename).
3. **Diagram inline colors must be promoted** or a light theme renders
   near-white text on a light canvas: node label `0xd6e4ee` (diagram.rs:389),
   tooltip black-alpha/white (diagram.rs:306/299/310), service fill
   `0x5b8db8` (diagram.rs:356), ingress fill `0x8d7ab8` (diagram.rs:361). These
   become `pal().node_text`, `pal().tooltip_bg`, `pal().tooltip_text`,
   `pal().service_fill`, `pal().ingress_fill`.
4. **`extreme_bg`** is currently inline `0x0a0d11` inside `apply()` (theme.rs:56).
   Promote to `pal().extreme_bg` or the light theme keeps a dark stripe.
5. **`dim()` dark-only assumptions**: besides the 6 direct call sites, any new
   code that mimics `rgb/5` must use `pal().dim` instead. Grep for `/ 5` and
   `rgb / 5` in the app crate when reviewing.
6. **e2e test does not construct `KindboardApp`** (tests/e2e.rs drives the
   worker via buses), so the `KindboardApp::new` signature change only affects
   `main.rs`. No test breakage expected.
7. **`CoreCommand` is exhaustive-matched** in worker.rs's `tokio::select!`
   (and possibly elsewhere). Adding `SetTheme` will surface every `match`
   that must handle it — compile errors are the guardrail; add the handler
   everywhere the compiler points.
8. **`Settings` is `PartialEq`** and has a manual `Default`; both must include
   the new `theme` field or tests comparing `Settings::default()` will fail.
9. **`AtomicU8` index safety**: `pal()` must never index out of bounds; keep
   `CURRENT` private and only write it through `set_index`/`set_theme` (both
   take a `ThemeId`, so the value is always 0..=2).

---

## 6. Test spec (for the QA engineer, TDD order)

1. `ThemeId::from_id("dark"|"Dark"|"DARK")` → `Some(Dark)`; `"light"`,
   `"high-contrast"` → mapped; `"purple"`/`""` → `None`.
2. `ThemeId::id()`/`label()` exact strings as §1.
3. `dim()` per theme (use `Color32::from_rgb(100, 100, 100)` as a probe):
   - Dark → `(20, 20, 20)` (100/5).
   - HighContrast → `(50, 50, 50)` (100/2).
   - Light → round to `(224, 224, 224)` (100 + (255-100)*0.80).
4. Palette completeness: for every symbol in §3's "from" column, the
   corresponding `Palette` field exists (a compile-time/static assertion or a
   unit test listing `pal()` field names).
5. `Settings` round-trip: `{"remember_last_wizard":true,"poll_interval_secs":5}`
   (no `theme`) deserializes with `theme == None`; a serialized `Settings` with
   `theme == None` omits the key; a `Settings` with an unknown key
   `{"foo":1}` round-trips with `foo` preserved in `other`.
6. `load_persisted_theme()` against a temp `DataDir` (write a settings.json
   with `"theme":"light"`, point `DataDir::new` at it) → `Light`; missing file
   → `Dark`.
7. Responsive smoke (headless, via the existing `--screenshot` harness): all
   three themes × {1280×800, 960×600, 640×480}; assert no view emits an
   overflow (no horizontal scrollbar in the central panels, modals fully
   visible, toolbar/tab bar wrap or scroll).

---

## 7. R8 text-visibility pairing (implemented amendments)

User rule: **letters must always be visible on buttons and highlighted
components**, in every theme. Implemented as:

1. **Widget fg strokes** (in `apply_theme`): `widgets.hovered.bg_fill = accent`,
   `widgets.active.bg_fill = accent_hover` (darker than the accent), both with
   `fg_stroke = Stroke(1.0, pal.on_accent)`. `accent_hover` values are chosen
   so the on-accent text keeps **≥4.4:1** on every fill in every theme
   (measured: hovered dark 7.06 / light 4.44 / HC 6.72; active dark 4.85 /
   light 5.78 / HC 7.58). `open` (combo boxes) keeps its default fill AND
   default text — no fg override (a white-on-grey combo would be invisible).
   `inactive`/`noninteractive` keep the base `Visuals::dark()/light()` text
   colors (readable on `bg_raised`/`bg`).
2. **Selection text** (egui 0.36): `Selection` has no `fg_stroke` — the
   selected-text color IS `selection.stroke`. It is now `Stroke(1.0,
   pal.on_accent)` (was `accent`, which put accent text on an accent fill).
3. **Filled buttons**: every explicitly filled button pairs an explicit
   label color:
   - accent fills → `pal().on_accent` (per-theme white/black, always legible
     on the accent),
   - red/amber destructive fills → `util::on_fill(fill)` — black on bright
     fills, white on dark fills, by relative luminance (ITU-R BT.709,
     threshold 150): Dark amber → black; Dark/Light red → white; Light
     amber → white; High-Contrast red → white.
   `util::on_fill(Color32) -> Color32` is the single helper; do not inline
   luminance logic elsewhere.
4. **Selected tab** (`render_tab_bar`) keeps explicit `pal().on_accent` text;
   the per-theme palettes make it legible on all three selection fills.

### egui-0.36 API-name corrections applied during implementation

- `ComboBox::from_id_salt` (not `from_id_source`) — used in the top-bar theme
  picker.
- `ScrollArea::id_salt` (not `id_source`) — wizard/tab-bar scroll areas.
- `Panel::{min_size,max_size}` exist on `egui::Panel` (0.36) and are set on
  the deps right panel + the cluster-tab left/right/bottom panels.
- `TextWrapMode::Wrap` set via `ui.style_mut().wrap_mode = Some(..)` inside
  the deps install log and ops output scroll areas.
