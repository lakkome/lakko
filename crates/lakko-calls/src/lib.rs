// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Audio/video calls for Lakko.
//!
//! Implements client-side call mechanics: SFrame (RFC 9605) media encryption,
//! adaptive jitter buffer, codec negotiation (Opus / AV1-SVC / VP9-SVC / H.264
//! fallback), and ICE/STUN/TURN with QUIC and TURN-over-TLS fallbacks.
//!
//! Media keys are derived from MLS exporters (`lakko/call/v1`) — see
//! `docs/adr/0001-mls-only.md` and `docs/adr/0003-livekit-fork.md`.
//!
//! ## Phase 3 scaffolding scope
//!
//! Today's surface is **keying only** — the SFrame frame
//! cipher + the jitter buffer + ICE plumbing land in later
//! sub-sprints. What ships now:
//!
//!   * [`sframe`] — per-call media key seed from a
//!     [`lakko_mls::Group`] exporter; per-participant key
//!     derivation via HKDF over the seed + sender leaf index.
//!     Counter-based per-frame nonce composition matches
//!     RFC 9605 §4.4.
//!
//! Frame encryption ([`sframe::ParticipantKey::seal`] /
//! [`sframe::ParticipantKey::open`]) lands alongside the
//! LiveKit-fork media-path integration in Phase 3.2.

#![forbid(unsafe_code)]

pub mod error;
pub mod sframe;

pub use error::{Error, Result};
pub use sframe::{CallKey, ParticipantKey, SFRAME_EXPORT_LABEL};
