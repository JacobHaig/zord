# First-run setup wizard — design

**Date:** 2026-06-10 · **Status:** approved · **Sub-project 2 of the premium-UX pass** (36b)

## Goal

A guided, fully-skippable first-launch experience that leaves the app
genuinely ready: intent → tuned defaults, mic picked and *heard working*,
permissions explained where they matter, the model optionally pre-downloaded,
and Discord configured if that's why the user came.

## Decisions (user-confirmed)

- **Intent tunes defaults and routes steps** — answers change real settings
  and decide which screens appear.
- **Everything skippable**: per-step Skip, a "Skip setup" escape on step 1,
  re-runnable from Settings → About ("Run setup again").

## Lifecycle

- New setting `setup_complete: bool` (serde default `false`).
- MainApp renders the wizard as a top overlay while a `show_wizard` signal is
  true (initialized from `!setup_complete`). Finishing **or** skipping sets
  `setup_complete = true` and saves. Existing installs see it once (two
  clicks to dismiss) — acceptable, no migration.
- Settings → About: **"Run setup again"** sets `show_wizard = true`.

## Architecture

- New module **`crates/zord-gui/src/wizard.rs`** (keeps main.rs from
  growing): `SetupWizard` component + `apply_intents` (pure, unit-tested).
- Props: `settings`, `show_wizard`, `engine: Engine`, `devices: Vec<String>`,
  `me_level: Signal<f32>`, `models: Signal<Vec<ModelInfo>>`,
  `model_progress: Signal<Option<(String, u8)>>`, `notice`.
- Steps are computed per-render from the intent selections:
  `Welcome → Intent → Microphone → [System audio: macOS + meetings] →
  Model → [Discord: discord intent + feature] → Ready`.
- **Reuse over rebuild**: the Discord step embeds the existing
  `IntegrationsSettings`; the model step drives the existing
  `ModelCmd::Download` + progress events; the mic meter is the existing
  `Meter` component fed by `Event::Level`.
- Styling: tokens only; wizard rules live in the components layer of
  `style.css` (`.wizard-card`, `.wizard-dots`, `.intent-card`, …).

## Steps

1. **Welcome** — one-liner + privacy promise; *Set up Zord* / *Skip setup*.
2. **Intent** — multi-select cards: **Meetings on this computer** ·
   **Discord calls** · **Just my voice**; plus a **low-powered machine**
   checkbox. Applied on Next via `apply_intents`:
   - voice-only (voice ∧ ¬meetings) → `capture_mode = "mic"`;
     meetings → `capture_mode = "both"`.
   - low-power → `live_transcription = false`, `model = "small.en"`.
   - discord → routes the Discord step + the final CTA.
3. **Microphone** — device picker (writes `input_device`) + a **Test
   microphone** start/stop button driving a live level meter. Engine gains
   `RecorderCmd::MicTestStart { device } / MicTestStop`: mic capture that
   emits the existing `Level` events *without* creating a session (the OS
   mic-permission prompt fires here, where the user expects it). A real
   recording Start stops any running test.
4. **System audio** *(macOS, meetings intent)* — explains the Screen
   Recording permission; **Open System Settings** deep-link
   (`x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture`);
   notes the relaunch. Windows/voice-only: step not shown.
5. **Model** — shows the recommended transcription model (low-power →
   `small.en`, else the default turbo-q5), sets it, and offers **Download
   now** (existing progress UI) or *later — downloads on first record*.
6. **Discord** *(discord intent + `discord` build)* — embedded
   `IntegrationsSettings` (token, user ID, Test connection, Invite bot).
   In a featureless build the step shows the build note instead.
7. **Ready** — summary of what was configured; CTA names the right button
   (*Record* / *Record Discord*). Finish → `setup_complete = true`.

## Engine addition (the only non-UI change)

`control_loop` keeps `mic_test: Option<(Microphone, Arc<AtomicBool>, JoinHandle)>`
between commands: `MicTestStart` (re)starts a `Microphone::start_with` capture
whose frames run through `smooth_level` and emit throttled
`Event::Level { source: Me }`; `MicTestStop` (and any session `Start`) tears
it down. No store, no WAV, no session row.

## Testing

- zord-config: `setup_complete` default test.
- wizard.rs: `apply_intents` unit tests (voice/meetings/low-power matrix).
- Engine: compile-level (mic test isn't headless-verifiable); manual GUI pass
  for the flow, skips, re-run entry, and the live meter.
