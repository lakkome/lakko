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

#![forbid(unsafe_code)]
