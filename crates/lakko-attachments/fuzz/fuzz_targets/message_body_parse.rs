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

#![no_main]

use lakko_attachments::fuzz_api::{media_metadata_within_bounds, parse_message_body};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_message_body(data);
    assert!(media_metadata_within_bounds(data));
});
