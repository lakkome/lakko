// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the group-invite preview **plaintext** decoder (ADR-0028
//! v2, the sealed preview of a `/g/` capability).
//!
//! `group_invite_decode` cannot reach this parser: on random input the
//! Ed25519 signature never verifies and the ChaCha20-Poly1305 open never
//! succeeds, so the preview plaintext decoder behind them is dead code to
//! that target. The plaintext is still attacker-authored in practice (the
//! issuer of a link writes it, and whoever holds the link's secret opens
//! it), so the version gate, the two one-byte length prefixes, UTF-8
//! validation, the display-name character table, the member count and the
//! trailing-bytes refusal are fed here directly through
//! `lakko_contacts::fuzz_api`.
//!
//! Asserts: no panic, and anything the decoder accepts also passes the
//! compose-side validation and re-encodes to the identical bytes (the
//! encoding is canonical, so decoder and encoder cannot disagree).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert_ne!(
        lakko_contacts::fuzz_api::group_invite_preview_plaintext(data),
        Some(false),
        "a preview the decoder accepts must validate and re-encode identically",
    );
});
