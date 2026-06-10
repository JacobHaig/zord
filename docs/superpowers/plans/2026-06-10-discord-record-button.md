# Record Discord Button Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the buried "Discord call" capture-mode dropdown option with a dedicated **Record Discord** button in the sidebar foot, driven by an explicit `integration` flag on `RecorderCmd::Start`.

**Architecture:** The GUI's record handler takes a `bool` (normal vs Discord); the engine is told per-recording instead of inferring from the sticky `capture_mode` setting. The `"discord"` capture mode is removed and migrated; a settings toggle can hide the button.

**Tech Stack:** Rust workspace, Dioxus 0.7 (`crates/zord-gui`), serde config (`crates/zord-config`). Spec: `docs/superpowers/specs/2026-06-10-discord-record-button-design.md`.

---

### Task 0: Commit the pending component refactor

The working tree holds an unstaged, already-gate-passing refactor of
`crates/zord-gui/src/main.rs` + `engine.rs` (MainApp split into components).
This plan edits the same files, so that work must be committed first or the
commits will mix.

**Files:**
- Modify: none (commit only)

- [ ] **Step 1: Confirm the tree state is only the refactor**

Run: `git status --short`
Expected: `M crates/zord-gui/src/engine.rs` and `M crates/zord-gui/src/main.rs` only.

- [ ] **Step 2: Commit it**

```bash
git add crates/zord-gui/src/engine.rs crates/zord-gui/src/main.rs
git commit -m "refactor(gui): split MainApp into focused #[component]s

Verbatim subtree extractions (IconRail, SessionsSidebar, SessionToolbar,
Summary/Compressed panels, SpeakerLegend, NotesDrawer, confirm dialogs,
SettingsOverlay + per-tab panes). Signals/engine/callbacks passed as
handles; adds PartialEq for Engine (all handles are clones of the one
spawned engine) so it can be a component prop. MainApp: 2046 -> 952 lines."
```

---

### Task 1: Config — `discord_record_button` setting + capture-mode migration

**Files:**
- Modify: `crates/zord-config/src/lib.rs` (Settings struct ~line 130, Default impl ~line 490, `load()` ~line 610, tests at end)

- [ ] **Step 1: Write the failing migration test**

Append inside the existing `#[cfg(test)] mod tests` block at the end of
`crates/zord-config/src/lib.rs`:

```rust
    #[test]
    fn discord_capture_mode_migrates_to_both() {
        // The "discord" capture mode was replaced by the Record Discord
        // button; leftover configs must fall back to local capture.
        let mut s: Settings = serde_json::from_str(r#"{ "capture_mode": "discord" }"#).unwrap();
        s.apply_migrations();
        assert_eq!(s.capture_mode, "both");
        // Other modes pass through untouched.
        let mut s: Settings = serde_json::from_str(r#"{ "capture_mode": "mic" }"#).unwrap();
        s.apply_migrations();
        assert_eq!(s.capture_mode, "mic");
        // The new button toggle defaults on.
        assert!(s.discord_record_button);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p zord-config discord_capture_mode -- --nocapture`
Expected: FAIL — `no method named apply_migrations` / `no field discord_record_button`.

- [ ] **Step 3: Add the setting field**

In `crates/zord-config/src/lib.rs`, directly after the `discord_announce`
field:

```rust
    /// Show the dedicated "Record Discord" button in the sidebar (Phase 30f).
    /// The button additionally requires the `discord` build feature and saved
    /// credentials; this toggle lets users hide it outright.
    #[serde(default = "default_true")]
    pub discord_record_button: bool,
```

And in `impl Default for Settings`, after `discord_announce: true,`:

```rust
            discord_record_button: true,
```

- [ ] **Step 4: Factor migrations out of `load()` and add the new one**

In `load()`, replace the inline summary-model migration block

```rust
        // Migrate away from models removed for non-commercial licensing so an
        // upgraded install doesn't keep pointing at one. (Reverb segmentation is
        // handled by SegmentationModel::parse_or_default falling back to pyannote.)
        if REMOVED_SUMMARY_MODELS.contains(&s.summary_model.as_str()) {
            tracing::info!(
                "summary model '{}' is non-commercial and was removed; resetting to default",
                s.summary_model
            );
            s.summary_model = default_summary_model();
        }
        s
```

with

```rust
        s.apply_migrations();
        s
```

and add the method to `impl Settings` (right below `load()`):

```rust
    /// One-time migrations for values left behind by removed features, applied
    /// on every load (idempotent).
    fn apply_migrations(&mut self) {
        // Migrate away from models removed for non-commercial licensing so an
        // upgraded install doesn't keep pointing at one. (Reverb segmentation is
        // handled by SegmentationModel::parse_or_default falling back to pyannote.)
        if REMOVED_SUMMARY_MODELS.contains(&self.summary_model.as_str()) {
            tracing::info!(
                "summary model '{}' is non-commercial and was removed; resetting to default",
                self.summary_model
            );
            self.summary_model = default_summary_model();
        }
        // The "discord" capture mode became the Record Discord button
        // (June 2026); leftover configs fall back to default local capture.
        if self.capture_mode == "discord" {
            self.capture_mode = "both".to_string();
        }
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p zord-config`
Expected: all PASS (3 existing + the new one).

- [ ] **Step 6: Commit**

```bash
git add crates/zord-config/src/lib.rs
git commit -m "feat(config): discord_record_button setting + discord capture-mode migration"
```

---

### Task 2: Engine — explicit `integration` flag on Start

**Files:**
- Modify: `crates/zord-gui/src/engine.rs` (`RecorderCmd::Start` enum ~line 318, control_loop Start arm ~line 2081)

- [ ] **Step 1: Add the field to `RecorderCmd::Start`**

In the `pub enum RecorderCmd` definition, after `live: bool,`:

```rust
        /// Start an integration (Discord) session instead of local capture —
        /// set by the Record Discord button. The `ZORD_DISCORD` /
        /// `ZORD_FAKE_INTEGRATION` env vars still force it (dev path).
        integration: bool,
```

- [ ] **Step 2: Use the flag in `control_loop`**

In the `RecorderCmd::Start { ... } =>` arm, add `integration,` to the
destructuring list (after `live,`). Then replace this block:

```rust
                // Integration session when the capture mode is "discord" (Phase
                // 30d UI) or a dev trigger is set — `ZORD_DISCORD` (real provider)
                // / `ZORD_FAKE_INTEGRATION` (fake). Else a normal recording.
                let discord_mode = zord_config::Settings::load().capture_mode == "discord";
                if discord_mode && !cfg!(feature = "discord") {
                    // Config written by a discord build, opened in one without
                    // the engine: refuse rather than silently record a fake
                    // (empty) session.
                    let _ = ev.send(Event::Status(Status::Error(
                        "capture mode is Discord but this build doesn't include the Discord engine — pick another capture mode in Settings → Recording".into(),
                    )));
                    continue;
                }
                let integration = discord_mode
                    || std::env::var("ZORD_DISCORD").is_ok()
                    || std::env::var("ZORD_FAKE_INTEGRATION").is_ok();
```

with:

```rust
                // Integration session when the Record Discord button asked for
                // one, or a dev trigger forces it — `ZORD_DISCORD` (real
                // provider) / `ZORD_FAKE_INTEGRATION` (fake). The button only
                // renders in discord builds, so the old "discord mode in a
                // featureless build" guard is gone with the mode itself.
                let integration = integration
                    || std::env::var("ZORD_DISCORD").is_ok()
                    || std::env::var("ZORD_FAKE_INTEGRATION").is_ok();
```

(If `rustfmt` re-wrapped the comment since this plan was written, locate the
block by grepping `discord_mode` in `control_loop` — it is the only use.)

- [ ] **Step 3: Check it fails to compile only at the GUI call site**

Run: `cargo check -p zord-gui 2>&1 | grep -E '^error' | head -5`
Expected: one error — `main.rs` missing the `integration` field at the
`RecorderCmd::Start { ... }` send in `on_record` (fixed in Task 3). The
engine itself must contribute no other errors.

- [ ] **Step 4: No commit yet** — Task 2 and Task 3 compile together; commit at the end of Task 3.

---

### Task 3: GUI — the button, the flag, mute-hiding

**Files:**
- Modify: `crates/zord-gui/src/main.rs` (signals ~line 466, `on_record` ~line 826, `discord_button` derivation ~line 870, SessionsSidebar call ~line 1085, SessionsSidebar component ~line 1380, sidebar foot ~line 1542)
- Modify: `crates/zord-gui/src/style.css` (after `.record.muted`, ~line 108)

- [ ] **Step 1: Add the `recording_discord` signal in MainApp**

Next to the mute signals (`let mut mic_muted = ...`):

```rust
    // Whether the current recording is an integration (Discord) session —
    // set at Start; drives hiding the local-capture mute buttons.
    let mut recording_discord = use_signal(|| false);
```

- [ ] **Step 2: Parameterize `on_record` by the integration flag**

Change the closure signature from `move |_|` to `move |integration: bool|`,
and inside the `else` (start) branch:
- after `sys_muted.set(false);` add:

```rust
                recording_discord.set(integration);
```

- add `integration,` to the `RecorderCmd::Start { ... }` send (after
  `live: s.live_transcription,`).

- [ ] **Step 3: Derive the button's visibility in MainApp**

Next to the existing `let mic_in_capture = ...` lines:

```rust
    // Record Discord button (spec 2026-06-10): discord build + credentials
    // saved + not hidden by the Integrations toggle.
    let discord_button = cfg!(feature = "discord")
        && !settings.read().discord_bot_token.is_empty()
        && !settings.read().discord_user_id.trim().is_empty()
        && settings.read().discord_record_button;
```

- [ ] **Step 4: Thread the new props through `SessionsSidebar`**

In the `SessionsSidebar { ... }` call inside MainApp's rsx, add (next to
`recording,`):

```rust
                discord_button,
                recording_discord: *recording_discord.read(),
```

In the `fn SessionsSidebar(...)` component props, change

```rust
    on_record: EventHandler<MouseEvent>,
```

to

```rust
    on_record: EventHandler<bool>,
```

and add (next to `recording: bool,`):

```rust
    discord_button: bool,
    recording_discord: bool,
```

- [ ] **Step 5: Update the sidebar foot**

In `SessionsSidebar`'s `div { class: "sidebar-foot",` block:

1. Mute buttons hide for Discord sessions — change both conditions:
   - `if recording && system_in_capture {` → `if recording && system_in_capture && !recording_discord {`
   - `if recording && mic_in_capture {` → `if recording && mic_in_capture && !recording_discord {`
2. Insert the Discord button **before** the Record button:

```rust
                    if !recording && discord_button {
                        button {
                            class: "record discord",
                            title: "Record the Discord voice channel you're in (via your bot)",
                            onclick: move |_| on_record.call(true),
                            {icon("headphones")}
                            "Record Discord"
                        }
                    }
```

3. The Record button's onclick: `onclick: move |e| on_record.call(e),` →
   `onclick: move |_| on_record.call(false),`

- [ ] **Step 6: Style the button**

In `crates/zord-gui/src/style.css`, after the `.record.muted` rule:

```css
/* The dedicated Discord record button (blurple, sibling of Record). */
.record.discord { background: #5865f2; }
```

- [ ] **Step 7: Build both flavors**

Run: `cargo check -p zord-gui && cargo check -p zord-gui --features discord`
Expected: both succeed, no warnings.

- [ ] **Step 8: Commit (Tasks 2+3 together)**

```bash
git add crates/zord-gui/src/engine.rs crates/zord-gui/src/main.rs crates/zord-gui/src/style.css
git commit -m "feat(gui): Record Discord button + explicit integration flag on Start"
```

---

### Task 4: Settings UI — Integrations toggle in, dropdown option out

**Files:**
- Modify: `crates/zord-gui/src/main.rs` (`IntegrationsSettings` component, `AudioInputSettings` component)

- [ ] **Step 1: Add the toggle to `IntegrationsSettings`**

After the announce toggle's trailing `p { class: "field-note", ... }`
(the one about the "recording started" message), insert:

```rust
            div { class: "field-row",
                label { class: "field-label", "Show the Record Discord button" }
                button {
                    class: if settings.read().discord_record_button { "toggle on" } else { "toggle" },
                    onclick: move |_| {
                        let mut s = settings.peek().clone();
                        s.discord_record_button = !s.discord_record_button;
                        let _ = s.save();
                        settings.set(s);
                    },
                    if settings.read().discord_record_button { "On" } else { "Off" }
                }
            }
            p { class: "field-note",
                "The sidebar button appears once a bot token and user ID are saved."
            }
```

- [ ] **Step 2: Remove the discord option from the capture dropdown**

In `AudioInputSettings`, delete this option:

```rust
                    if cfg!(feature = "discord") {
                        option { value: "discord", selected: settings.read().capture_mode == "discord", "Discord call (via your bot)" }
                    }
```

and delete its explainer below the select:

```rust
            if settings.read().capture_mode == "discord" {
                p { class: "field-note",
                    "All audio (including you) comes from Discord — no mic or desktop capture. Set up the bot under Settings → Integrations."
                }
            }
```

- [ ] **Step 3: Full gate**

Run:
```bash
cargo fmt --all && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo clippy -p zord-gui --features discord -- -D warnings && \
cargo test --workspace
```
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add crates/zord-gui/src/main.rs
git commit -m "feat(gui): Record-Discord-button toggle in Integrations; drop discord capture mode from dropdown"
```

---

### Task 5: Docs + live verification

**Files:**
- Modify: `docs/discord-integration.md` (user-flow steps 5–6)
- Modify: `docs/PLAN.md` (Phase 30 list — add 30f entry)

- [ ] **Step 1: Update the user flow in `docs/discord-integration.md`**

Replace:

```markdown
5. Set the capture mode (Settings → Recording) to **"Discord"** and **join a
   voice channel** in a server the bot is in.
6. Press **Record**. The bot finds the channel you're in, **joins it**, and
```

with:

```markdown
5. **Join a voice channel** in a server the bot is in.
6. Press **Record Discord** (the blurple button above Record — it appears once
   the token and user ID are saved; hideable in Settings → Integrations). The
   bot finds the channel you're in, **joins it**, and
```

- [ ] **Step 2: Note the change in `docs/PLAN.md`**

After the `30e` bullet in the Phase 30 list, add:

```markdown
- **30f ✅ DONE (June 2026)** Dedicated **Record Discord** button (sidebar
  foot, shown when the build + credentials + an Integrations toggle allow it);
  `RecorderCmd::Start` carries an explicit `integration` flag instead of the
  engine re-reading `capture_mode`; the `"discord"` capture mode was removed
  from the dropdown and old configs migrate to `"both"`. Mute buttons no
  longer render during integration sessions (nothing local to mute). Spec:
  `docs/superpowers/specs/2026-06-10-discord-record-button-design.md`.
```

- [ ] **Step 3: Commit**

```bash
git add docs/discord-integration.md docs/PLAN.md
git commit -m "docs: Record Discord button flow (30f)"
```

- [ ] **Step 4: Live verification (user-driven)**

```bash
cargo run -p zord-gui --features discord
```

Checklist: button appears (credentials already saved) · toggle in
Settings → Integrations hides/shows it · capture dropdown has no Discord
entry · join voice + press Record Discord → bot joins + announcement posts ·
no mute buttons while recording · Stop (or leave voice) ends the session ·
normal Record still records mic+desktop.

---

## Self-review

- **Spec coverage:** button + visibility rules (T3), settings toggle (T4),
  dropdown removal + migration (T1, T4), explicit Start flag + guard removal
  (T2), mute-hiding (T3), docs/testing (T1 test, T4 gate, T5 live). ✓
- **Placeholders:** none — every step carries the code or exact command. ✓
- **Type consistency:** `on_record: EventHandler<bool>` matches
  `on_record.call(false)` / `.call(true)`; `integration: bool` named the same
  in enum, destructuring, and send; `discord_record_button` matches field,
  Default, toggle, and visibility check. ✓
