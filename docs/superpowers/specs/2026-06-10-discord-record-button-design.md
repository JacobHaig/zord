# Record Discord button — design

**Date:** 2026-06-10 · **Status:** approved (approach A)

## Problem

Starting a Discord recording requires flipping the capture-mode dropdown in
Settings → Recording to "Discord call (via your bot)" and back. That hides the
app's headline feature behind a settings dance, and leaves the engine inferring
*what kind* of recording to make from a sticky setting instead of being told.

## Decision (user-confirmed)

A dedicated **Record Discord** button next to the normal Record button —
not a source switcher, not auto-detection. One explicit entry point per
recording kind:

- **Record** → local capture, honoring the capture-mode setting
  (mic + desktop / mic / desktop / one app).
- **Record Discord** → integration session via the user's bot.

`"discord"` leaves the capture-mode dropdown entirely (single entry point).

## UI

- Sidebar foot, directly **above** the Record button: `[🎧 Record Discord]`,
  same shape as Record, Discord-blurple accent so the two read as siblings.
- **Visible only when** all of:
  1. the build has the `discord` feature,
  2. a bot token **and** user ID are saved (Settings → Integrations),
  3. the new **"Show Record Discord button"** toggle (Settings → Integrations,
     default **on**) is on.
- While any recording runs, the foot shows Stop (as today); the Discord button
  hides.
- During a **Discord** recording the mic/desktop **mute buttons do not appear**
  — there is no local capture to mute. (Fixes today's behavior, where they
  appear but do nothing meaningful.)

## Plumbing

- `RecorderCmd::Start` gains **`integration: bool`**. The engine's
  `control_loop` uses the flag instead of re-reading `capture_mode`;
  `ZORD_DISCORD` / `ZORD_FAKE_INTEGRATION` env vars still force it (dev path).
  The "discord capture mode in a featureless build" guard becomes unreachable
  and is removed with the mode.
- The Record-button handler (`on_record`) is parameterized by the flag; the
  Discord button passes `true`. MainApp keeps a `recording_discord` signal so
  the foot knows to hide the mute buttons for that session.
- Settings:
  - new `discord_record_button: bool` (serde default **true**), toggle rendered
    in Settings → Integrations under the announce toggle;
  - `"discord"` option + explainer removed from the capture dropdown
    (Settings → Recording);
  - `Settings::load()` migrates a leftover `capture_mode == "discord"` to
    `"both"` (same pattern as the removed-summary-model migration).
- Engine guards unchanged otherwise: missing credentials still error before a
  session row is created; provider construction is untouched.

## Edge cases

- Unconfigured / non-discord build → button absent; Integrations tab remains
  the setup path.
- Press with the bot not invited / user not in voice → existing engine errors
  surface in the status bar (unchanged).
- Old config with `capture_mode = "discord"` → silently migrated to `"both"`
  on first load; the user starts Discord recordings via the button thereafter.

## Testing

- Unit: `Settings::load()` migration of `capture_mode == "discord"`.
- Build/lint matrix: default + `--features discord` (+ existing CI gate).
- Manual: live GUI test — button appears after credentials are saved, starts a
  bot session, mutes hidden, Stop / leave-voice both end it; normal Record
  unaffected.
