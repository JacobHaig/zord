# UI polish + theming system — design

**Date:** 2026-06-10 · **Status:** approved · **Sub-project 1 of 2**
(sub-project 2 = the first-run setup wizard, specced separately)

## Goal

Make the existing design feel premium — finished states, consistent geometry,
motion, depth — and give users real theme control, on a modular, composable
token system. No layout or navigation changes.

## 1. Token layer (`crates/zord-gui/src/style.css`)

One `:root` block is the single source of the visual language:

- **Spacing**: `--sp-1..6` = 4 / 8 / 12 / 16 / 24 / 32 px.
- **Radius**: `--r-sm` 6 · `--r-md` 10 · `--r-lg` 14.
- **Elevation**: `--elev-1/2/3` (soft layered shadows for cards, menus, dialogs).
- **Motion**: `--t-fast` 120ms (hovers) · `--t-pop` 180ms (entrances) ·
  `--ease` cubic-bezier(0.2, 0.8, 0.2, 1).
- **Focus**: `--ring` (2px accent outline w/ offset, used via `:focus-visible`).
- **Type scale**: 11/12/13/14/16 with defined weights; headings tightened.

The file is reorganized into labeled layers — `tokens → primitives →
components → screens` — and every rule below tokens consumes them (no raw px
radii / hex colors in component rules).

## 2. Color roles (split today's overloaded names)

| Token | Role | Default | User-settable |
|---|---|---|---|
| `--accent` | interactive: active nav/tabs, primary buttons, toggles-on, focus ring, selection | `#4cc2ff` (today's cyan) | ✅ |
| `--danger` | record + destructive actions (fixed; recording = red is iconic) | `#ff4d6d` (today's `--accent`) | ❌ |
| `--me` / `--others` | transcript channel colors (text tint, meters, replay) | `#4cc2ff` / `#ffb454` | ✅ |
| `--accent-fg` | readable text on accent (black/white) | computed | auto |

Hover/soft variants derive with `color-mix()` (supported by WKWebView on
macOS 13+ and evergreen WebView2), so one user value drives the family.
`.record.discord` keeps its fixed blurple.

## 3. Button system (CSS-level composition, no markup churn)

A shared base — token padding/radius, `--t-fast` transitions, hover lift
(`filter`/background shift), pressed compression (`transform: scale(0.98)`),
`:focus-visible` ring, disabled fade — applied to the existing button families
via selector groups (`.record`, `.mbtn`, `.tbtn`, `.row-btn`, `.rail-btn`,
`.stab`, `.toggle`, `.chip`, `.gen-item`, `.export-menu-item`, `.notes-tab`,
`.close-btn`). Variants stay per-family. Rust markup keeps its class names.

## 4. Motion & depth

- Entrances (180ms fade + 2% scale): settings overlay card, confirm dialogs,
  Generate/Export dropdown menus, toasts, the jobs panel, notes drawer.
- Session-row action buttons fade in on row hover (today they pop).
- Consistent `--elev-*` shadows on panel/card/menu/dialog surfaces.
- Styled scrollbars (thin, themed thumb) and `::selection` color.
- The Record button gets a subtle gradient + hover glow + press state; the
  recording dot keeps its pulse.

## 5. Theme panel (Settings → Theme)

- **Accent**: 6 curated swatches — cyan `#4cc2ff`, blurple `#5865f2`, coral
  `#ff7059`, green `#3ecf8e`, violet `#a78bfa`, amber `#ffb454` — plus a
  custom hex input with live preview.
- **Me / Others**: small swatch rows + hex inputs each.
- Existing badge-tint toggle (unchanged).
- **Reset to defaults** button.

Settings (zord-config, serde defaults, empty string = built-in default):
`theme_accent`, `theme_me`, `theme_others`. Hex values are validated
(`#rrggbb`); invalid input is ignored (last valid wins).

**Apply mechanism**: MainApp writes the custom properties onto the `.app`
root's `style` attribute from settings — instant live preview, no restart,
zero CSS regeneration. A small Rust helper picks `--accent-fg` (black/white)
by relative luminance so any custom accent stays readable.

## 6. Out of scope (deliberate)

The web dashboard's own CSS · light mode · density/font-size options (cheap
later via tokens) · the first-run wizard (sub-project 2).

## Testing

- zord-config: roundtrip/default test for the three new fields + hex
  validation helper test.
- zord-gui: unit test for the luminance → fg helper.
- Full fmt/clippy/test gate; visual pass in the running app (user-judged).
