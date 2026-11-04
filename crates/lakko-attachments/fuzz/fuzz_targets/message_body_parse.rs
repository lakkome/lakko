// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `parse` (the receiver-side MLS Application message-body
//! decoder, `fuzz_api::parse_message_body`). The fuzz input is a message
//! body: a malicious or compromised group member can craft arbitrary
//! envelope bytes, which reach this decode path on receipt before any
//! rendering. The harness drives the full receive chain — the
//! `serde_json::from_slice::<Envelope>` decode → the schema-version gate
//! → the bare-UTF-8 `from_utf8_lossy` fallback → the per-ref base64
//! decoders (`decode_blob_id` / `decode_key` / `decode_nonce`, each a
//! base64 decode + fixed length check). Any panic / overflow /
//! slice-out-of-bounds here is a vulnerability.
//!
//! ADR-0047: the same bytes also drive the media-metadata surface — the
//! lenient `kind` / `duration_ms` / `waveform` / `width` / `height` /
//! `thumb` decode and its bounds — through
//! `fuzz_api::media_metadata_within_bounds`, a property oracle asserted
//! here: every media field that survives parse is within its bound, and
//! re-composing the parsed refs then parsing again yields the same refs.
//! ADR-0087 section 14: the `channel_invite` card is bounded the same way
//! (`fuzz_api::channel_invite_within_bounds`). The reaction op (an explicit
//! add / remove and an optional target-author key) is checked by
//! `fuzz_api::reaction_within_bounds`.

#![no_main]

use lakko_attachments::fuzz_api::{
    channel_invite_within_bounds, forward_within_bounds, media_metadata_within_bounds,
    parse_message_body, reaction_within_bounds, reply_within_bounds, resend_within_bounds,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_message_body(data);
    assert!(media_metadata_within_bounds(data));
    // ADR-0052: a reply's snapshots survive parse only within their bounds,
    // and the bound is idempotent under compose_reply -> parse_body.
    assert!(reply_within_bounds(data));
    assert!(forward_within_bounds(data), "ADR-0052: a forward that survives parse is within its bounds and idempotent");
    // ADR-0073: a re-send marker survives parse only whole and within its
    // bound, and the bound is idempotent under compose_chat -> parse_body.
    assert!(resend_within_bounds(data));
    // ADR-0087: a channel invitation card survives parse only within its
    // bound, and compose_channel_invite -> parse_body keeps it.
    assert!(channel_invite_within_bounds(data));
    // UX reactions-and-emoji section 17: a reaction that survives parse has
    // a bounded emoji and an explicit add / remove op, and its target author
    // survives compose_reaction -> parse_body whole or not at all.
    assert!(reaction_within_bounds(data));
});
