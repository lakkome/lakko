// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `header_of` (v2 envelope plaintext
//! chain-header parser).
//!
//! The chain header is plaintext on wire so the audit
//! path doesn't need the recovery phrase. That makes it
//! the cheapest attacker-touchable surface in the backup
//! restore flow — a malicious storage backend can hand
//! arbitrary bytes to a client that's only "peeking" at
//! the version/prev_hash before deciding whether to ask
//! the user for their phrase. Any panic / overflow on
//! arbitrary input is a vulnerability. Pinned in the
//! audit-pack alongside G-11 close-out.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_backup::header_of;

fuzz_target!(|data: &[u8]| {
    let _ = header_of(data);
});
