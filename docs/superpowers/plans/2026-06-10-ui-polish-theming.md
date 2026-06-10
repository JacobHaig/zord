# UI Polish + Theming System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Token-driven visual polish (states, motion, depth) across the GUI plus a user-facing Theme panel (accent / Me / Others colors, presets + custom hex), with the default look unchanged.

**Architecture:** All styling flows from a `:root` token block in `style.css` (spacing/radius/elevation/motion/focus + color roles). The overloaded colors split into roles — `--accent` (interactive, user-set, defaults to today's cyan), `--danger` (fixed red), `--me`/`--others` (channel colors, user-set) — so by default nothing changes visually. User colors land as CSS custom properties on the `.app` root's `style` attribute, written from three new settings.

**Tech Stack:** CSS custom properties + `color-mix()` (WKWebView macOS 13+ / WebView2), Dioxus 0.7, zord-config serde settings.

Spec: `docs/superpowers/specs/2026-06-10-ui-polish-theming-design.md`.
Commits go straight to `develop` (user request), one per task, full gate before each.

---

### Task 1: CSS foundation — tokens, color roles, shared button base, motion

**Files:**
- Modify: `crates/zord-gui/src/style.css` (whole-file pass)

- [ ] **Step 1: Replace the `:root` block with the token layer**

Replace the current `:root { … }` (lines 2–12) with:

```css
/* ============================ 1. TOKENS ===================================
   The single source of the visual language. Components below consume tokens —
   no raw radii/colors/durations in component rules. User-themable values
   (--accent, --me, --others + their -fg pairs) are overridden at runtime via
   the .app root's style attribute (Settings → Theme). */
:root {
  /* surfaces */
  --bg: #0f1115;
  --panel: #171a21;
  --panel-2: #1e222b;
  --line: #262b36;
  --text: #e6e9ef;
  --muted: #8a93a6;

  /* color roles */
  --accent: #4cc2ff;        /* interactive: active nav, primary, toggles, focus */
  --accent-fg: #06222f;     /* readable text on accent (auto-set when themed) */
  --danger: #ff4d6d;        /* record + destructive — fixed, never themed */
  --me: #4cc2ff;            /* transcript channel: you */
  --me-fg: #06222f;
  --others: #ffb454;        /* transcript channel: them */
  --others-fg: #06222f;
  --discord: #5865f2;       /* the Record Discord button — fixed brand color */

  /* derived (one accent value drives the family) */
  --accent-soft: color-mix(in srgb, var(--accent) 14%, transparent);
  --accent-glow: color-mix(in srgb, var(--accent) 35%, transparent);
  --danger-glow: color-mix(in srgb, var(--danger) 45%, transparent);

  /* geometry */
  --sp-1: 4px; --sp-2: 8px; --sp-3: 12px; --sp-4: 16px; --sp-5: 24px; --sp-6: 32px;
  --r-sm: 6px; --r-md: 10px; --r-lg: 14px;

  /* elevation */
  --elev-1: 0 1px 2px rgba(0,0,0,0.25);
  --elev-2: 0 4px 14px rgba(0,0,0,0.35);
  --elev-3: 0 12px 36px rgba(0,0,0,0.5);

  /* motion */
  --t-fast: 120ms;
  --t-pop: 180ms;
  --ease: cubic-bezier(0.2, 0.8, 0.2, 1);
}
```

- [ ] **Step 2: Apply the color-role substitutions**

Mechanical, verified by grep afterwards:

| Old | New | Where |
|---|---|---|
| `var(--accent)` | `var(--danger)` | `.mbtn.danger` (bg+border), `.dot.rec`, `.record` bg, `.rec-timer`, `.tbtn.ghost.danger:hover`, `.job-cancel:hover` |
| `var(--accent)` | keep `var(--accent)` | `.rail-brand` (brand mark follows the user accent) |
| `var(--me)` | `var(--accent)` | every *interactive* use: `.rail-btn.active`, `.rail-btn.jobs`, `.splitter:hover/.active`, `.rename-input`, `.stab.active`, `.slider`, `.toggle.on`, `.model-row.sel`, `.chip:hover/.on`, `.mbtn:hover`, `.dl-bar`, `.search:focus`, `.tbtn:hover/.busy`, `.gen-state.ok`, `.summary` border-left, `.session-filter:focus`, `.overview-sec[open]`, `.ledger-project[open]`, `.ledger-kind.k-action`, `.ledger-add-btn:hover`, `.ledger-edit-text`, `.chat-input:focus`, `.search-input-big:focus`, `.search-group-head:hover .sg-title`, `.search-hit:hover`, `.jobs-spin`, `.job-icon`, `.job-bar-fill`, `.speaker-name:focus`, `.line-edit`, `.play-btn.on`, `.prompt-edit:focus` |
| `var(--me)` | keep `var(--me)` | channel-true uses: `.meter-fill.me`, `.line.me .who`, `.chat-msg.user .chat-text`, `.badge.tint-sum` |
| `#06222f` text on accent/me/others fills | `var(--accent-fg)` on `.toggle.on`/`.chip.on`; `var(--me-fg)` on `.chat-msg.user .chat-text`; `var(--others-fg)` on `.record.muted` |
| `rgba(255,77,109,…)` pulse literals | `var(--danger-glow)` | `.dot.rec` shadow + `@keyframes pulse` |
| `#5865f2` | `var(--discord)` | `.record.discord` |

- [ ] **Step 3: Add the primitives layer (after tokens, before components)**

```css
/* ========================= 2. PRIMITIVES ==================================
   Shared interactive behavior for every button family. Composition happens
   here at the CSS level — markup keeps its existing class names. */
:is(.record, .mbtn, .tbtn, .row-btn, .rail-btn, .stab, .toggle, .chip,
    .gen-item, .export-menu-item, .notes-tab, .close-btn, .ledger-add-btn,
    .job-cancel, .panel-toggle) {
  transition: background var(--t-fast) var(--ease), border-color var(--t-fast) var(--ease),
              color var(--t-fast) var(--ease), box-shadow var(--t-fast) var(--ease),
              transform var(--t-fast) var(--ease), filter var(--t-fast) var(--ease);
}
:is(.record, .mbtn, .tbtn, .row-btn, .rail-btn, .stab, .toggle, .chip,
    .gen-item, .export-menu-item, .notes-tab, .close-btn, .ledger-add-btn,
    .job-cancel, .panel-toggle):active:not(:disabled) {
  transform: scale(0.97);
}
:is(.row-btn, .stab, .gen-item, .export-menu-item, .close-btn, .panel-toggle):hover {
  background: var(--panel-2);
  color: var(--text);
}
:is(button, input, select, textarea, summary):focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
  border-radius: var(--r-sm);
}
:is(button, input, select, textarea):focus:not(:focus-visible) { outline: none; }
:disabled { opacity: 0.45; cursor: not-allowed; }
::selection { background: var(--accent-glow); }

/* themed scrollbars */
::-webkit-scrollbar { width: 10px; height: 10px; }
::-webkit-scrollbar-thumb { background: var(--line); border-radius: 8px; border: 2px solid transparent; background-clip: padding-box; }
::-webkit-scrollbar-thumb:hover { background: var(--muted); border: 2px solid transparent; background-clip: padding-box; }
::-webkit-scrollbar-track { background: transparent; }

/* entrance animations */
@keyframes pop-in {
  from { opacity: 0; transform: translateY(4px) scale(0.98); }
  to   { opacity: 1; transform: translateY(0) scale(1); }
}
@keyframes fade-in {
  from { opacity: 0; }
  to   { opacity: 1; }
}
```

- [ ] **Step 4: Apply entrances + state/depth polish to components**

Exact rule changes (add to the existing selectors):

```css
/* entrances */
.overlay { animation: fade-in var(--t-pop) var(--ease); }
.overlay-card, .confirm-card, .jobs-card { animation: pop-in var(--t-pop) var(--ease); box-shadow: var(--elev-3); }
.gen-menu, .export-menu { animation: pop-in var(--t-pop) var(--ease); box-shadow: var(--elev-2); }
.notice { animation: pop-in var(--t-pop) var(--ease); box-shadow: var(--elev-1); }
.notes-drawer.open { box-shadow: var(--elev-2); }

/* session rows: actions fade instead of popping; row hover surface */
.session { transition: background var(--t-fast) var(--ease), border-color var(--t-fast) var(--ease); }
.session:hover { background: var(--panel-2); }
.session-actions { display: flex; opacity: 0; transition: opacity var(--t-fast) var(--ease); }
.session:hover .session-actions, .session.active .session-actions { opacity: 1; }

/* the Record button earns its place as THE button */
.record {
  background: linear-gradient(180deg, color-mix(in srgb, var(--danger) 92%, white), var(--danger));
  box-shadow: var(--elev-1);
}
.record:hover:not(.stop):not(.mute):not(.muted):not(.discord) {
  box-shadow: 0 0 0 4px var(--danger-glow), var(--elev-1);
}
.record.discord {
  background: linear-gradient(180deg, color-mix(in srgb, var(--discord) 92%, white), var(--discord));
}
.record.discord:hover { box-shadow: 0 0 0 4px color-mix(in srgb, var(--discord) 40%, transparent), var(--elev-1); }
```

Note: `.session-actions` currently uses `display: none` → `display: flex` on
hover; replace that pair with the opacity rules above (search for the existing
`.session:hover .session-actions` rule and remove its `display` toggle).

Then normalize geometry through the components layer: replace raw
`border-radius: 6px/10px/14px` with `var(--r-sm/md/lg)` and label the file's
sections with the layer banners (`1. TOKENS`, `2. PRIMITIVES`,
`3. COMPONENTS`, `4. SCREENS` — screens = unlock, settings overlay, search
view, jobs panel blocks).

- [ ] **Step 5: Verify default look unchanged + states work**

Run: `cargo build -p zord-gui && cargo run -p zord-gui` (quick visual pass:
hover a session row, open a menu, tab-focus a button, check Record button)
Expected: identical palette to before; new hover/press/focus/entrance behavior.

- [ ] **Step 6: Commit**

```bash
git add crates/zord-gui/src/style.css
git commit -m "style(gui): token layer, color roles, shared button states, motion + depth"
```

---

### Task 2: Config — theme settings + hex validation (TDD)

**Files:**
- Modify: `crates/zord-config/src/lib.rs`

- [ ] **Step 1: Write the failing test** (append inside the existing `#[cfg(test)] mod tests`)

```rust
    #[test]
    fn theme_settings_roundtrip_and_hex_validation() {
        // Defaults: unset (empty) = use the built-in palette.
        let s = Settings::default();
        assert_eq!(s.theme_accent, "");
        assert_eq!(s.theme_me, "");
        assert_eq!(s.theme_others, "");
        // Validation: exactly #rrggbb.
        assert!(is_valid_hex_color("#4cc2ff"));
        assert!(is_valid_hex_color("#FFB454"));
        assert!(!is_valid_hex_color("4cc2ff"));
        assert!(!is_valid_hex_color("#4cc2f"));
        assert!(!is_valid_hex_color("#4cc2fg"));
        assert!(!is_valid_hex_color("#4cc2ff00"));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p zord-config theme_settings -- --nocapture`
Expected: FAIL — no field `theme_accent`, no fn `is_valid_hex_color`.

- [ ] **Step 3: Implement**

Settings struct (next to `badge_tint`):

```rust
    /// Theme overrides (Settings → Theme): `#rrggbb`, empty = built-in default.
    /// `theme_accent` drives the interactive color; `theme_me`/`theme_others`
    /// drive the transcript channel colors. Danger/record red is never themed.
    #[serde(default)]
    pub theme_accent: String,
    #[serde(default)]
    pub theme_me: String,
    #[serde(default)]
    pub theme_others: String,
```

`impl Default` additions: `theme_accent: String::new(), theme_me: String::new(), theme_others: String::new(),`

Free function (near `restrict_to_owner`):

```rust
/// Strictly `#rrggbb` — anything else is rejected (theme inputs keep the last
/// valid value rather than injecting arbitrary text into a style attribute).
pub fn is_valid_hex_color(s: &str) -> bool {
    s.len() == 7
        && s.starts_with('#')
        && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p zord-config`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zord-config/src/lib.rs
git commit -m "feat(config): theme_accent/theme_me/theme_others + strict hex validation"
```

---

### Task 3: GUI plumbing — readable foreground + root style binding (TDD)

**Files:**
- Modify: `crates/zord-gui/src/main.rs` (helpers near `capture_sources`; MainApp root div ~line 1095)

- [ ] **Step 1: Write the failing test** (append to the `mod tests` at the end of `engine.rs`? No — these helpers live in `main.rs`, which has no test mod yet; add one at the end of `main.rs`)

```rust
#[cfg(test)]
mod theme_tests {
    use super::*;

    #[test]
    fn readable_fg_picks_contrast() {
        assert_eq!(readable_fg("#ffffff"), "#06222f"); // light bg → dark text
        assert_eq!(readable_fg("#4cc2ff"), "#06222f"); // cyan is light
        assert_eq!(readable_fg("#1a1a2e"), "#ffffff"); // dark bg → white text
        assert_eq!(readable_fg("#5865f2"), "#ffffff"); // blurple is dark enough
        assert_eq!(readable_fg("not-a-color"), "#06222f"); // garbage → default
    }

    #[test]
    fn theme_style_only_emits_valid_overrides() {
        let mut s = zord_config::Settings::default();
        assert_eq!(theme_style(&s), "");
        s.theme_accent = "#5865f2".into();
        let css = theme_style(&s);
        assert!(css.contains("--accent: #5865f2;"));
        assert!(css.contains("--accent-fg: #ffffff;"));
        s.theme_me = "junk".into(); // invalid → ignored
        assert!(!theme_style(&s).contains("--me:"));
        s.theme_others = "#3ecf8e".into();
        assert!(theme_style(&s).contains("--others: #3ecf8e;"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p zord-gui theme_ -- --nocapture`
Expected: FAIL — `readable_fg` / `theme_style` not found.

- [ ] **Step 3: Implement the helpers** (near `capture_sources` at the bottom of `main.rs`)

```rust
/// Black-or-white text for a `#rrggbb` background, by relative luminance —
/// keeps any user-picked theme color readable. Garbage input gets the
/// default dark ink (matches the built-in cyan's pairing).
fn readable_fg(hex: &str) -> &'static str {
    if !zord_config::is_valid_hex_color(hex) {
        return "#06222f";
    }
    let v = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f32 / 255.0;
    let lin = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let lum = 0.2126 * lin(v(1)) + 0.7152 * lin(v(3)) + 0.0722 * lin(v(5));
    if lum > 0.35 {
        "#06222f"
    } else {
        "#ffffff"
    }
}

/// CSS custom-property overrides for the `.app` root from the theme settings.
/// Only valid `#rrggbb` values are emitted (inputs are also validated at the
/// settings layer; this is the second gate before a style attribute).
fn theme_style(s: &Settings) -> String {
    let mut css = String::new();
    for (var, value) in [
        ("--accent", &s.theme_accent),
        ("--me", &s.theme_me),
        ("--others", &s.theme_others),
    ] {
        if zord_config::is_valid_hex_color(value) {
            css.push_str(&format!("{var}: {value}; {var}-fg: {}; ", readable_fg(value)));
        }
    }
    css.trim_end().to_string()
}
```

- [ ] **Step 4: Bind it to the app root**

In MainApp's rsx, the root `div { class: "app", …`: add (as the first
attribute, before the mouse handlers):

```rust
            style: "{theme_style(&settings.read())}",
```

- [ ] **Step 5: Run tests + build**

Run: `cargo test -p zord-gui theme_ && cargo check -p zord-gui`
Expected: PASS, clean check.

- [ ] **Step 6: Commit**

```bash
git add crates/zord-gui/src/main.rs
git commit -m "feat(gui): runtime theme application — readable_fg + root custom-property overrides"
```

---

### Task 4: Theme panel — swatches, custom hex, reset

**Files:**
- Modify: `crates/zord-gui/src/main.rs` (`ThemeSettings` component)
- Modify: `crates/zord-gui/src/style.css` (swatch styles)

- [ ] **Step 1: Replace `ThemeSettings` with the full panel**

```rust
/// Curated accent presets: (name, hex). Cyan first = the built-in default.
const ACCENT_PRESETS: [(&str, &str); 6] = [
    ("Cyan", "#4cc2ff"),
    ("Blurple", "#5865f2"),
    ("Coral", "#ff7059"),
    ("Green", "#3ecf8e"),
    ("Violet", "#a78bfa"),
    ("Amber", "#ffb454"),
];

/// One row of theme control: a label, preset swatches, and a hex input.
/// `current` empty = the built-in default (first preset highlighted).
#[component]
fn ColorRow(
    label: String,
    default_hex: String,
    current: String,
    presets: Vec<(&'static str, &'static str)>,
    on_pick: EventHandler<String>,
) -> Element {
    let effective = if current.is_empty() { default_hex.clone() } else { current.clone() };
    rsx! {
        div { class: "field-row",
            label { class: "field-label", "{label}" }
            div { class: "swatch-row",
                for (name, hex) in presets {
                    button {
                        key: "{hex}",
                        class: if effective.eq_ignore_ascii_case(hex) { "swatch on" } else { "swatch" },
                        style: "background: {hex};",
                        title: "{name}",
                        onclick: move |_| on_pick.call(hex.to_string()),
                    }
                }
                input {
                    class: "swatch-hex",
                    placeholder: "{default_hex}",
                    value: "{current}",
                    oninput: move |e: FormEvent| {
                        let v = e.value().trim().to_string();
                        if v.is_empty() || zord_config::is_valid_hex_color(&v) {
                            on_pick.call(v);
                        }
                    },
                }
            }
        }
    }
}

/// Theme settings: accent / Me / Others colors (presets + custom hex, applied
/// live), the badge-tint toggle, and reset.
#[component]
fn ThemeSettings(settings: Signal<Settings>) -> Element {
    let set = move |apply: fn(&mut Settings, String), v: String| {
        let mut s = settings.peek().clone();
        apply(&mut s, v);
        let _ = s.save();
        settings.set(s);
    };
    rsx! {
        section { class: "settings-section",
            h3 { "Theme" }
            p { class: "field-note", "Colors apply instantly. The hex fields accept any #rrggbb; text on your color stays readable automatically. Record stays red — that one means something." }
            ColorRow {
                label: "Accent".to_string(),
                default_hex: "#4cc2ff".to_string(),
                current: settings.read().theme_accent.clone(),
                presets: ACCENT_PRESETS.to_vec(),
                on_pick: move |v| set(|s, v| s.theme_accent = v, v),
            }
            ColorRow {
                label: "Me (your channel)".to_string(),
                default_hex: "#4cc2ff".to_string(),
                current: settings.read().theme_me.clone(),
                presets: ACCENT_PRESETS.to_vec(),
                on_pick: move |v| set(|s, v| s.theme_me = v, v),
            }
            ColorRow {
                label: "Others (their channel)".to_string(),
                default_hex: "#ffb454".to_string(),
                current: settings.read().theme_others.clone(),
                presets: ACCENT_PRESETS.to_vec(),
                on_pick: move |v| set(|s, v| s.theme_others = v, v),
            }
            div { class: "field-row",
                label { class: "field-label", "Session badges: tint by meaning" }
                button {
                    class: if settings.read().badge_tint { "toggle on" } else { "toggle" },
                    onclick: move |_| {
                        let mut s = settings.peek().clone();
                        s.badge_tint = !s.badge_tint;
                        let _ = s.save();
                        settings.set(s);
                    },
                    if settings.read().badge_tint { "Tinted" } else { "Mono" }
                }
            }
            p { class: "field-note", "The summary / compressed / speakers badges in the sidebar are color-coded by meaning (cyan / amber / green) so you can read a session at a glance. Turn off for a calmer, monochrome look." }
            div { class: "field",
                button {
                    class: "mbtn ghost",
                    onclick: move |_| {
                        let mut s = settings.peek().clone();
                        s.theme_accent = String::new();
                        s.theme_me = String::new();
                        s.theme_others = String::new();
                        let _ = s.save();
                        settings.set(s);
                    },
                    "Reset colors to defaults"
                }
            }
        }
    }
}
```

- [ ] **Step 2: Swatch CSS** (components layer of `style.css`)

```css
/* Theme swatches (Settings → Theme) */
.swatch-row { display: flex; align-items: center; gap: var(--sp-2); }
.swatch {
  width: 24px; height: 24px; border-radius: 50%;
  border: 2px solid transparent; cursor: pointer; padding: 0;
  box-shadow: inset 0 0 0 1px rgba(0,0,0,0.25);
}
.swatch:hover { transform: scale(1.12); }
.swatch.on { border-color: var(--text); box-shadow: 0 0 0 3px var(--accent-glow); }
.swatch-hex {
  width: 92px; background: var(--panel-2); color: var(--text);
  border: 1px solid var(--line); border-radius: var(--r-sm);
  padding: 4px 8px; font-size: 12px; font-family: ui-monospace, monospace;
}
.swatch-hex:focus { border-color: var(--accent); outline: none; }
```

- [ ] **Step 3: Build + visual check**

Run: `cargo run -p zord-gui` → Settings → Theme → click swatches, watch the
app retheme live; type a custom hex; reset.
Expected: instant application, defaults restore on reset.

- [ ] **Step 4: Commit**

```bash
git add crates/zord-gui/src/main.rs crates/zord-gui/src/style.css
git commit -m "feat(gui): Theme panel — accent/Me/Others presets + custom hex, live apply, reset"
```

---

### Task 5: Gate, docs, push

- [ ] **Step 1: Full gate**

```bash
cargo fmt --all && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo clippy -p zord-gui --features discord -- -D warnings && \
cargo test --workspace
```
Expected: all green.

- [ ] **Step 2: PLAN.md** — under section 9 (productionization), append a phase entry:

```markdown
### Phase 36 — UI polish + theming (sub-project 1 of the premium-UX pass)
✅ **36a DONE (June 2026).** Token layer in `style.css` (spacing/radius/
elevation/motion/focus + color roles split: `--accent` interactive (themable),
`--danger` fixed red, `--me`/`--others` channels (themable), `--discord`
fixed); shared button-state primitives via selector groups (no markup churn);
entrance animations, hover/press/focus states, themed scrollbars, elevation;
Theme panel: 6 accent presets + custom hex for accent/Me/Others with
luminance-picked readable foregrounds, live apply via root custom properties,
reset. Spec: `docs/superpowers/specs/2026-06-10-ui-polish-theming-design.md`.
**36b (next)**: first-run guided setup wizard — specced separately.
```

- [ ] **Step 3: Commit + push**

```bash
git add docs/PLAN.md
git commit -m "docs(plan): Phase 36a UI polish + theming done"
git push origin develop
```

---

## Self-review

- **Spec coverage**: tokens (T1.1), role split incl. fg vars + discord (T1.2),
  button primitives + focus + scrollbars + selection (T1.3), motion/depth +
  record treatment + session-action fade (T1.4), settings + validation (T2),
  fg helper + root binding (T3), panel w/ presets + hex + reset (T4), tests in
  T2/T3, gate in T5. Out-of-scope list respected (no light mode, no web CSS).
- **Placeholders**: none — every step carries code or exact commands.
- **Type consistency**: `is_valid_hex_color` (zord-config, pub) used in T3/T4;
  `readable_fg`/`theme_style` defined T3, bound T3.4; `ColorRow.on_pick:
  EventHandler<String>` matches `.call(String)` uses; preset array type
  `[(&str, &str); 6]` matches `Vec<(&'static str, &'static str)>` via
  `.to_vec()`.
