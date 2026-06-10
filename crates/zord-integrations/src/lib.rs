//! Platform integrations for Zord.
//!
//! An *integration* is a capture source that hands Zord **separate, already-
//! identified audio feeds — one per participant** (vs. the mixed system loopback
//! that needs diarization). Discord is the first: its voice gateway delivers each
//! participant as a distinct RTP stream, so we get per-speaker audio *and* real
//! names for free — no clustering pass.
//!
//! This crate currently holds only the **Phase 27 de-risking spike** (the
//! `discord-spike` binary, behind the `discord` feature), which proves per-user
//! audio can be received and decrypted under Discord's mandatory DAVE end-to-end
//! encryption. The `Integration` trait (Phase 29) and the production Discord
//! source (Phase 30) build on what the spike confirms.
//!
//! See `docs/PLAN.md` → "Platform integrations (Phases 27–31)".
//!
//! Phase 29 adds the **seam** to the default build: the [`Integration`] trait
//! (one identity-labeled audio stream per participant) plus a dependency-free
//! [`FakeProvider`] to validate the engine path before any network backend. The
//! Discord implementation (Phase 30) lands behind the `discord` feature; the
//! `discord-spike` bin (Phase 27) remains as the DAVE receive proof.

mod fake;
mod integration;
mod session;

pub use fake::FakeProvider;
pub use integration::{AudioStream, Integration, IntegrationEvent, Participant};
pub use session::{drive_session, EndReason};
