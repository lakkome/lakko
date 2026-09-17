// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `PushPayload::decode_json` (the on-device push-payload
//! parser). The fuzz input is the attacker-controlled push `data`
//! payload: APNs / FCM / UnifiedPush relay these bytes verbatim and the
//! client decodes them on wake-up before any further processing, so the
//! input is fully untrusted. The harness drives the full decode pipeline
//! — `serde_json::from_slice` → the schema-version gate → the kind
//! dispatch (`g` / `c` / `m` / `w` + field-presence rules) → the base64
//! pseudonym decode + 32-byte length check. Any panic / overflow /
//! slice-out-of-bounds here is a vulnerability.
//!
//! `c` (a call wake) shares `g`'s field shape and therefore `g`'s
//! decode arm, so it adds no new parser surface — but it is a distinct
//! dispatch tag, and enumerating it here keeps the oracle's stated
//! coverage honest against the code.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_push::fuzz_api::decode_push_payload(data);
});
