// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Lakko-native SFU signaling wire format.
//!
//! The frame definitions moved to the shared [`lakko_sfu_proto`]
//! crate in the SFU multi-party arc (subsprint 1) so the **server**
//! (this crate) and the **clients** (via `lakko-ffi-uniffi` +
//! `lakko-ffi-wasm`) parse and emit the exact same wire shape from
//! one definition — no per-platform hand-coded JSON parsers, no
//! drift between server and clients.
//!
//! This module re-exports the shared types so the rest of the SFU
//! keeps referring to `crate::protocol::{ClientFrame, ServerFrame,
//! …}` unchanged. The round-trip / kebab-case / unknown-kind tests
//! live with the definitions in `lakko-sfu-proto`.
//!
//! See the [`lakko_sfu_proto`] crate docs for the opacity invariant
//! (`payload_b64` is opaque to the SFU) and the full frame catalogue.

pub use lakko_sfu_proto::{
    peer_tag, ClientFrame, ErrorCode, PeerSnapshot, RelayTarget, ServerFrame, VideoGiven,
    VideoQuality, VideoWant, MAX_INBOUND_FRAME_BYTES, MAX_VIDEO_GIVEN, MAX_VIDEO_WANTS,
    SIGNALING_SUBPROTOCOL, TOKEN_SUBPROTOCOL_PREFIX,
};
