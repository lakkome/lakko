// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sync primitives for Lakko: hybrid logical clock + edit chain +
//! drafts. Phase 1.12 ships the **single-device** variants — every
//! op is local, the [`HybridLogicalClock`] is per-process, drafts
//! live in `lakko-storage`. The structures are intentionally chosen
//! so Phase 2's DeviceGroup multi-device sync (per ADR-0001 and
//! plan §7) is a layer **above** these types, not a redesign of them.
//!
//! ## Module map
//!
//! - [`hlc`] — Hybrid Logical Clock (Kulkarni–Demirbas–Browne 2014).
//!   Strictly-monotonic stamps that combine wall-clock milliseconds
//!   with a logical counter so events ordered within and across
//!   devices.
//! - [`edit`] — append-only edit chain per message, last-write-wins
//!   by [`Hlc`]. Includes Update + Delete (tombstone) ops; the
//!   resolver returns [`Resolved::Visible`], [`Resolved::Deleted`]
//!   or [`Resolved::Empty`].
//! - [`draft`] — per-group [`Draft`] with the same LWW rule. One
//!   draft per group, replaceable by a strictly-greater HLC.
//!
//! ## Why no character-level CRDT
//!
//! In Lakko's threat / UX model only the original sender ever edits
//! their own message — there are no concurrent multi-author edits
//! to merge, so the "merge two simultaneous typing sessions"
//! complexity of Yjs / Automerge is overkill. Flat
//! `Vec<EditOp>` + HLC ordering gives the right answer for every
//! realistic scenario at a fraction of the wire-format and
//! complexity cost.
//!
//! ## Out of scope (Phase 2+)
//!
//! - **DeviceGroup MLS substrate** — each device a separate leaf in
//!   a personal MLS group, used as the carrier for sync events
//!   (Phase 2.3).
//! - **Cross-device sync** of edits, read receipts, drafts, and
//!   conversation order through DeviceGroup application messages
//!   (Phase 2.4).
//! - **History catch-up bundle** — HPKE-envelope from an old device
//!   to a freshly-paired new one carrying the last N messages
//!   (Phase 2.5).
//! - **Read receipts** wire format — same-shape LWW state machine
//!   on top of [`Hlc`], plumbed through DeviceGroup (Phase 2.4).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod draft;
pub mod edit;
pub mod hlc;

pub use draft::{merge_drafts, Draft};
pub use edit::{EditChain, EditOp, Resolved};
pub use hlc::{Hlc, HybridLogicalClock};

/// Fuzz-only entry points. Marked `#[doc(hidden)]` — this is the sole
/// harness surface for `crates/lakko-sync/fuzz/`, **not** part of the
/// supported public API.
///
/// `lakko-sync` carries no on-the-wire byte parser of its own (the
/// DeviceGroup substrate owns deserialization, Phase 2). The
/// attacker-relevant surface is therefore the LWW / HLC *logic* that
/// processes a peer device's ops after AEAD-open: a malicious or
/// compromised peer supplies adversarial [`Hlc`] stamps and op
/// orderings and must not be able to drive the local clock backwards
/// (which would let it reorder or resurrect edits — an LWW
/// ordering-integrity break). This module thus exposes a *property
/// oracle* rather than a parse wrapper.
#[doc(hidden)]
pub mod fuzz_api {
    use crate::{EditChain, EditOp, Hlc, HybridLogicalClock};

    /// Upper bound on decoded events — caps allocation so a short
    /// fuzzer input cannot drive an unbounded `Vec`.
    const MAX_EVENTS: usize = 1024;

    /// One decoded receive event.
    enum Ev {
        /// Local emit at an arbitrary (fuzzer-chosen) wall-clock reading.
        Tick(u64),
        /// Remote edit op received from a peer (carries its own HLC).
        Remote(EditOp),
    }

    /// Read a big-endian `u64` from `s[*i..]`, consuming up to 8 bytes
    /// (right-zero-extended when fewer remain). Always advances `*i` by
    /// the number consumed and never panics.
    fn take_u64(s: &[u8], i: &mut usize) -> u64 {
        let n = s.len().saturating_sub(*i).min(8);
        let mut buf = [0u8; 8];
        buf[..n].copy_from_slice(&s[*i..*i + n]);
        *i += n;
        u64::from_be_bytes(buf)
    }

    /// Decode the fuzzer bytes into a bounded event sequence. Total:
    /// any byte string maps to a valid (possibly empty) event list.
    fn decode(bytes: &[u8]) -> Vec<Ev> {
        let mut evs = Vec::new();
        let mut i = 0usize;
        while i < bytes.len() && evs.len() < MAX_EVENTS {
            let ctrl = bytes[i];
            i += 1;
            match ctrl & 0b11 {
                // Local tick at a fuzzer-controlled wall-clock value.
                0 | 3 => evs.push(Ev::Tick(take_u64(bytes, &mut i))),
                // Remote Update carrying an adversarial HLC + body.
                1 => {
                    let pt = take_u64(bytes, &mut i);
                    let lc = take_u64(bytes, &mut i);
                    let want = if i < bytes.len() {
                        let l = bytes[i] as usize;
                        i += 1;
                        l
                    } else {
                        0
                    };
                    let n = want.min(bytes.len().saturating_sub(i));
                    let body = String::from_utf8_lossy(&bytes[i..i + n]).into_owned();
                    i += n;
                    evs.push(Ev::Remote(EditOp::Update {
                        hlc: Hlc::new(pt, lc),
                        body,
                    }));
                },
                // Remote Delete (tombstone) carrying an adversarial HLC.
                _ => {
                    let pt = take_u64(bytes, &mut i);
                    let lc = take_u64(bytes, &mut i);
                    evs.push(Ev::Remote(EditOp::Delete {
                        hlc: Hlc::new(pt, lc),
                    }));
                },
            }
        }
        evs
    }

    /// Replay an adversarial receive sequence and report whether every
    /// ordering-integrity invariant held. The harness turns a `false`
    /// return into a crash via `assert!`, so a future regression that
    /// breaks one of these surfaces immediately.
    ///
    /// Invariants checked:
    /// 1. **HLC monotonicity** — neither `tick` nor `merge_remote` ever
    ///    issues a stamp `<` the clock's previously-issued stamp, no
    ///    matter what `remote` HLC a peer supplies (the load-bearing
    ///    guarantee: a peer cannot wind the local clock backwards).
    /// 2. **`merge` == append** — splitting the op set, merging the
    ///    halves, and resolving yields the same view as resolving the
    ///    whole set in one `EditChain` (the documented commutativity of
    ///    `EditChain::merge`).
    /// 3. **resolve idempotence** — `current()` is pure (two successive
    ///    calls agree).
    ///
    /// Order-independence under HLC *ties* is deliberately NOT asserted:
    /// `EditChain::current` tie-breaks equal HLCs by vector position
    /// (documented `max_by_key` behaviour), so reordering a tie
    /// legitimately changes the winner — that is design, not a defect.
    #[must_use]
    pub fn replay_and_check(bytes: &[u8]) -> bool {
        let evs = decode(bytes);

        // (1) HLC monotonicity across the mixed local/remote sequence.
        let mut clock = HybridLogicalClock::new();
        let mut prev = Hlc::ZERO;
        let mut ops: Vec<EditOp> = Vec::new();
        for ev in &evs {
            let issued = match ev {
                Ev::Tick(wall) => clock.tick(*wall),
                Ev::Remote(op) => {
                    ops.push(op.clone());
                    clock.merge_remote(op.hlc(), 0)
                },
            };
            if issued < prev {
                return false;
            }
            prev = issued;
        }

        // (2) merge == append-equivalence on the collected remote ops.
        let mid = ops.len() / 2;
        let full = EditChain::from_ops(ops.clone());
        let mut split = EditChain::from_ops(ops[..mid].to_vec());
        split.merge(&EditChain::from_ops(ops[mid..].to_vec()));
        if full.current() != split.current() {
            return false;
        }

        // (3) resolve idempotence.
        let once = full.current();
        let twice = full.current();
        once == twice
    }
}

// ============================================================================
// fuzz_api oracle tests — verify the property oracle firsthand on stable
// (the libfuzzer target itself needs nightly + cargo-fuzz and is run by the
// nightly fuzz-quick CI workflow). These double as a regression guard: a
// future change that lets a peer wind the local clock backwards, or that
// breaks merge==append, flips one of these to `false` and fails the build.
// ============================================================================

#[cfg(test)]
mod fuzz_api_tests {
    use crate::fuzz_api::replay_and_check;

    // Encode one event in the `fuzz_api::decode` wire shape (the low two
    // bits of the control byte pick the variant).
    fn tick(wall: u64) -> Vec<u8> {
        let mut v = vec![0u8]; // ctrl & 0b11 == 0 -> Tick
        v.extend_from_slice(&wall.to_be_bytes());
        v
    }
    fn update(pt: u64, lc: u64, body: &[u8]) -> Vec<u8> {
        let cap = body.len().min(255);
        let mut v = vec![1u8]; // ctrl & 0b11 == 1 -> Update
        v.extend_from_slice(&pt.to_be_bytes());
        v.extend_from_slice(&lc.to_be_bytes());
        v.push(u8::try_from(cap).unwrap_or(0));
        v.extend_from_slice(&body[..cap]);
        v
    }
    fn delete(pt: u64, lc: u64) -> Vec<u8> {
        let mut v = vec![2u8]; // ctrl & 0b11 == 2 -> Delete
        v.extend_from_slice(&pt.to_be_bytes());
        v.extend_from_slice(&lc.to_be_bytes());
        v
    }

    #[test]
    fn invariants_hold_on_empty_input() {
        assert!(replay_and_check(&[]));
    }

    #[test]
    fn invariants_hold_under_saturating_adversarial_stamps() {
        // All-ones drives repeated ticks at wall == u64::MAX; the clock
        // saturates at (MAX, MAX) and monotonicity must still hold.
        assert!(replay_and_check(&[0xFF; 256]));

        // A remote op carrying (u64::MAX, u64::MAX) then local ticks:
        // merge_remote jumps to the ceiling, ticks saturate, the issued
        // stamps never go backwards.
        let mut blob = delete(u64::MAX, u64::MAX);
        blob.extend(tick(0));
        blob.extend(tick(u64::MAX));
        assert!(replay_and_check(&blob));
    }

    #[test]
    fn invariants_hold_on_a_realistic_edit_stream() {
        let mut blob = Vec::new();
        blob.extend(update(100, 0, b"hello"));
        blob.extend(update(200, 0, b"hello world"));
        blob.extend(delete(150, 0)); // out-of-order older delete
        blob.extend(tick(300));
        blob.extend(update(400, 5, b"final"));
        assert!(replay_and_check(&blob));
    }

    #[test]
    fn decoder_is_total_over_a_deterministic_byte_sweep() {
        // No RNG (Date.now / Math.random are banned in this codebase); a
        // deterministic u8 LCG-free pattern exercises every decoder branch
        // and the body-length slicing at a spread of lengths, asserting no
        // panic and that the invariants hold throughout.
        let mut b: u8 = 0;
        let mut bytes: Vec<u8> = Vec::new();
        for len in 0..1_500usize {
            assert!(replay_and_check(&bytes), "invariant broke at len {len}");
            b = b.wrapping_mul(31).wrapping_add(7);
            bytes.push(b);
        }
    }
}
