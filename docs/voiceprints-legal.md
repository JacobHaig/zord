# Voiceprints and biometric-privacy law — internal memo

> **Status:** internal research notes (2026-06), compiled from a multi-source
> research pass. **Not legal advice.** Phase 38 ships behind the `voiceprints`
> Cargo feature so the capability can be removed from release builds entirely
> if this picture sours. Review with counsel before marketing the feature.

## What we store

A "voiceprint" in Zord is a **speaker embedding**: a 192–512-dimension f32
vector produced by the same sherpa-onnx models the diarizer uses, stored in
the local SQLite DB. No enrollment audio clips are kept. Nothing is ever
uploaded — Zord has no server and no telemetry.

## Are speaker embeddings "biometric data"? Yes.

- **GDPR (EU):** embeddings processed *for the purpose of uniquely
  identifying a person* are special-category biometric data under Article 9.
  Processing requires explicit consent (Art. 9(2)(a)); data minimization
  (Art. 5(1)(c)) favors storing embeddings over audio.
- **Illinois BIPA (740 ILCS 14):** "voiceprint" is **explicitly listed** as a
  biometric identifier. §15(b) requires written notice of purpose + retention
  and written authorization *before* collection; §15(d) requires a published
  retention/destruction schedule. Private right of action; $1,000–$5,000
  statutory damages per violation (SB 2979, 2024: repeated captures of the
  same biometric count as one violation).
- **Texas CUBI (Bus. & Com. Code ch. 503):** "voiceprint" explicitly listed.
  Consent before capture; destruction within a year of the purpose expiring;
  no third-party disclosure. AG-only enforcement ($25k/violation), no class
  actions.
- **Washington (RCW 19.375):** voiceprints listed; the statute is triggered
  by *enrolling* a person into an identification system — which is exactly
  what persistent voiceprints do. AG enforcement only.

## The litigation wave is aimed at exactly this feature

AI meeting tools with speaker recognition are the active 2025–2026 BIPA
frontier, and the theory in every case is the same: diarization that produces
persistent voice embeddings = voiceprint collection without notice/consent.

- **Cruz v. Fireflies.AI** (C.D. Ill., Dec 2025; second suit Mar 2026) —
  plaintiff was a meeting participant who never had an account; the
  "Speaker Recognition" feature is the named violation.
- **Otter.ai** federal BIPA class action — voiceprints of Zoom participants
  who never consented.
- **Zaluda v. Apple** — class certified Jan 2026 over Siri "biometric feature
  vectors"; potentially 3M+ members.
- **Parker v. Verizon** (2024), **Whole Foods** (~$300k settlement, 2023),
  pending Microsoft Teams claims.

## Does local-only change the analysis? Substantially, but not to zero.

The strongest fact in Zord's favor: **the developer never collects,
receives, or possesses anyone's biometric data.** BIPA applies to entities
that "collect, capture, … or otherwise obtain" biometrics; with a fully
on-device pipeline there is nothing we ever obtain. An Illinois appellate
court dismissed BIPA claims against Apple over on-device Face ID, and
on-device processing is a recognized BIPA mitigation strategy. Vendor-
liability cases (e.g. Johnson v. NCR) all involved data reaching the
vendor's servers — not our architecture.

Caveats:
- The pure local-software question has **not been definitively adjudicated**.
- GDPR Recital 18 applies the regulation to those who "provide the means for
  processing"; a commercial developer is not fully out of scope even with
  zero data access. The *user* running Zord may themselves have obligations
  toward the people they record.
- Embeddings are **not safely anonymous**: model-inversion research (e.g.
  arXiv 2301.03206) reconstructs speaker-matched audio from embeddings with
  ~100% verification accuracy. ISO/IEC 24745 considers raw embeddings
  non-compliant with template non-invertibility. We must treat the vectors
  as identifying data, not hashes.

## Mitigations Zord builds in (Phase 38)

These mirror what plaintiffs/regulators look for, regardless of whether any
statute reaches us:

1. **Off by default; explicit opt-in** — a one-time plain-language consent
   dialog (purpose, local-only storage, retention, deletability) with an
   affirmative click, timestamped in settings (`voiceprints_consented_at`) —
   the written-authorization analog.
2. **Local-only, always** — voiceprints are never synced, exported, included
   in any backup we create, or transmitted. There is no server.
3. **Per-person "Forget this voice"** + a global "Forget all voices" —
   irreversible deletion, surfaced in the Speakers view and Settings.
4. **Data minimization** — embeddings only (a few KB/person), never
   enrollment audio; rolling cap of 8 samples per person; session embeddings
   deleted with their session (ON DELETE CASCADE).
5. **Retention statement in-app** (consent dialog + Speakers view): kept
   until the user deletes them or forgets the speaker.
6. **Kill-switch:** the entire capability is behind the `voiceprints` Cargo
   feature; omitting it from release builds removes the code paths, the UI,
   and any new data collection.

## Open questions for counsel (later)

- Should release builds geo-gate or add BIPA-verbatim notice for Illinois?
- Does the in-app consent of the *Zord user* need companion guidance about
  consent from *other meeting participants* (it's their biometrics)?
- EU distribution posture: are we comfortable relying on the user-as-
  controller / household-exemption framing for consumer use?
- Whether "voiceprint" branding in marketing increases exposure vs. neutral
  wording ("speaker matching").
