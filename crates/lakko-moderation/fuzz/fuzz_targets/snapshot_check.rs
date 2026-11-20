// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The suspension snapshot (ADR-0089 section 5, steps 4 and 5): a bucket, a
//! snapshot root and a snapshot log leaf decoded from bytes a moderation
//! service chose, and the whole bucket answer checked, for an account and
//! for a channel, under fixed keys the fuzzer does not hold, and a mark
//! proved from a bucket answer, a log entry answer and a moderator list cut
//! from the same bytes (`prove_standing`). Whatever decodes must re-encode
//! to the identical bytes; nothing may verify or prove.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_moderation::fuzz_api::snapshot_check(data);
});
