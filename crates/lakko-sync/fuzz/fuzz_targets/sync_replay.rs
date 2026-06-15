// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the multi-device sync receive path (`lakko-sync`).
//!
//! `lakko-sync` has no on-the-wire byte parser of its own; the
//! attacker-relevant surface is the LWW / HLC *logic* that processes a
//! peer device's ops after AEAD-open. A malicious or compromised peer
//! supplies adversarial [`Hlc`] stamps and op orderings and must not be
//! able to drive the local clock backwards (which would let it reorder
//! or resurrect edits — an ordering-integrity break).
//!
//! The harness decodes the fuzz input into such a receive sequence and
//! asserts the ordering-integrity invariants hold:
//!   1. HLC monotonicity (`tick` / `merge_remote` never go backwards);
//!   2. `EditChain::merge` == append (documented commutativity);
//!   3. `current()` resolve idempotence.
//! A violated invariant (a future regression) surfaces as a crash here.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_sync::fuzz_api::replay_and_check(data),
        "lakko-sync ordering-integrity invariant violated",
    );
});
