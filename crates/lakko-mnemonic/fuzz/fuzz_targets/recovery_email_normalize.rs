// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the recovery-address rule (ADR-0075 section 4, via
//! `fuzz_api::recovery_email_normalizes_stably`). An address is typed by a
//! person and sent to auth, and both sides normalise it once, so a
//! normalised address must normalise to itself; the bytes, when UTF-8, go
//! through `normalize_recovery_email` twice. Until 2026-11-26 this target
//! also opened the email vault, which ADR-0075 (amended) removed.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_mnemonic::fuzz_api::recovery_email_normalizes_stably(data),
        "normalising twice changed the address"
    );
});
