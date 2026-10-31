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
//! - [`synced`] — the multi-device fold of ADR-0058: chat folders plus the
//!   flags / read cursors / drafts / conversation order maps the
//!   DeviceGroup carries, one commutative, associative, idempotent
//!   [`SyncedState`] every client consumes over the FFI, its persistence
//!   codec, and the catch-up snapshot. The same fold holds the
//!   conversation-lineage registers (ADR-0073), the account's recovery
//!   email register (ADR-0075) and its own-username register (ADR-0076),
//!   each carried between devices by its own
//!   DeviceGroup service envelope, plus the saved-message register
//!   (ADR-0065) and the conversation-end register (ADR-0087), which ride
//!   the same change set as a folder edit.
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
pub mod synced;

pub use draft::{merge_drafts, Draft};
pub use edit::{EditChain, EditOp, Resolved};
pub use hlc::{Hlc, HybridLogicalClock};
pub use synced::{should_send_snapshot, SyncedState};

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
    use crate::synced::{
        compose_lineage_envelope, compose_link_record_envelope, compose_own_username_envelope,
        compose_recovery_email_envelope, decode_lineage_envelope, decode_link_record_envelope,
        decode_own_username_envelope, decode_recovery_email_envelope, ApplyOutcome, BoundsError,
        ConversationEnd, Stamped, SyncEntry,
    };
    use crate::{EditChain, EditOp, Hlc, HybridLogicalClock, SyncedState};

    /// Drive the four byte decoders of the synced state on arbitrary input
    /// and check what they accept (ADR-0073 R.5, ADR-0075, ADR-0076,
    /// ADR-0058):
    ///
    /// * [`decode_lineage_envelope`] — reached from a DeviceGroup service
    ///   envelope another device of the account sent. Whatever decodes must
    ///   re-encode to the identical bytes (the encoding is canonical) and
    ///   fold into a state whose own envelopes carry the same registers.
    /// * [`decode_recovery_email_envelope`] — the account's recovery email
    ///   register, reached the same way. Whatever decodes must re-encode to
    ///   the identical bytes, hold an address within its bounds (or the
    ///   removal), fold to exactly that register, and not apply twice.
    /// * [`decode_own_username_envelope`] — the account's own username
    ///   register, reached the same way, held to the same properties.
    /// * [`decode_link_record_envelope`] — the account's invite links
    ///   (ADR-0079), reached the same way. Whatever decodes must re-encode to
    ///   the identical bytes, hold records within their bounds, and fold so
    ///   that every revoked record it carries is held revoked.
    /// * [`SyncedState::from_bytes`] — reached from local persistence and
    ///   from the backup payload a storage tier serves. Whatever decodes
    ///   must persist to bytes that read back as the same state, its
    ///   recovery email and own-username registers must be valid and travel
    ///   by their envelopes, and its conversation-end registers must read
    ///   back, travel in the snapshot, and yield writes that take effect.
    ///
    /// None may panic. Returns `false` when a property is violated.
    #[must_use]
    pub fn decode_synced_and_check(bytes: &[u8]) -> bool {
        lineage_envelope_holds(bytes)
            && recovery_email_envelope_holds(bytes)
            && own_username_envelope_holds(bytes)
            && link_record_envelope_holds(bytes)
            && persisted_state_holds(bytes)
    }

    /// The link-record envelope (ADR-0079): canonical, within bounds, and a
    /// revoked record it carries is held revoked after the fold.
    fn link_record_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(entries) = decode_link_record_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        compose_link_record_envelope(&entries).as_deref() == Ok(bytes)
            && entries.iter().all(|e| e.value.validate().is_ok())
            && state.apply_link_record_envelope(bytes).is_ok()
            && state.all_link_records() == entries
            && entries.iter().all(|e| {
                !e.value.revoked
                    || state
                        .link_record(&e.value.link_id)
                        .is_some_and(|r| r.value.revoked)
            })
    }

    /// The lineage envelope: canonical, and it folds to exactly its own
    /// registers.
    fn lineage_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(entries) = decode_lineage_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        compose_lineage_envelope(&entries).as_deref() == Ok(bytes)
            && state.apply_lineage_envelope(bytes).is_ok()
            && state.all_lineage() == entries
            && state.lineage_envelopes() == vec![bytes.to_vec()]
    }

    /// The recovery-email envelope (ADR-0075): canonical, within its bounds,
    /// it folds to exactly its own register, and a re-send is not an update.
    fn recovery_email_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(register) = decode_recovery_email_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        compose_recovery_email_envelope(&register).as_deref() == Ok(bytes)
            && register.value.validate().is_ok()
            && state.apply_recovery_email_envelope(bytes) == Ok(true)
            && state.recovery_email_register().as_ref() == Some(&register)
            && state.recovery_email().is_some() != register.value.is_removed()
            && state.recovery_email_envelope().as_deref() == Some(bytes)
            && state.apply_recovery_email_envelope(bytes) == Ok(false)
    }

    /// The own-username envelope (ADR-0076): the same properties.
    fn own_username_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(register) = decode_own_username_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        compose_own_username_envelope(&register).as_deref() == Ok(bytes)
            && register.value.validate().is_ok()
            && state.apply_own_username_envelope(bytes) == Ok(true)
            && state.own_username_register().as_ref() == Some(&register)
            && state.own_username().is_some() != register.value.is_none()
            && state.own_username_envelope().as_deref() == Some(bytes)
            && state.apply_own_username_envelope(bytes) == Ok(false)
    }

    /// The persisted state: it reads back as itself, and each
    /// envelope-carried register is valid and rebuilt by its envelope.
    fn persisted_state_holds(bytes: &[u8]) -> bool {
        let Ok(state) = SyncedState::from_bytes(bytes) else {
            return true;
        };
        if SyncedState::from_bytes(&state.to_bytes()).as_ref() != Ok(&state) {
            return false;
        }
        if !conversation_ends_hold(&state) {
            return false;
        }
        let mut rebuilt = SyncedState::new();
        for envelope in state.lineage_envelopes() {
            if rebuilt.apply_lineage_envelope(&envelope).is_err() {
                return false;
            }
        }
        if let Some(envelope) = state.recovery_email_envelope() {
            if rebuilt.apply_recovery_email_envelope(&envelope).is_err() {
                return false;
            }
        }
        if let Some(envelope) = state.own_username_envelope() {
            if rebuilt.apply_own_username_envelope(&envelope).is_err() {
                return false;
            }
        }
        for envelope in state.link_record_envelopes() {
            if rebuilt.apply_link_record_envelope(&envelope).is_err() {
                return false;
            }
        }
        rebuilt.all_lineage() == state.all_lineage()
            && rebuilt.all_link_records() == state.all_link_records()
            && rebuilt.recovery_email_register() == state.recovery_email_register()
            && rebuilt.own_username_register() == state.own_username_register()
            && !state
                .recovery_email_register()
                .is_some_and(|r| r.value.validate().is_err())
            && !state
                .own_username_register()
                .is_some_and(|r| r.value.validate().is_err())
    }

    /// The conversation-end registers of a persisted state (ADR-0087 section
    /// 9): what the reads list is what the state holds, the snapshot carries
    /// every one, and a register a writer composes takes effect, which is
    /// what lets a client read `None` as "nothing to tell".
    fn conversation_ends_hold(state: &SyncedState) -> bool {
        // `Ok(None)` is an answer too; a refused reference is the failure.
        let takes_effect = |composed: Result<Option<Stamped<ConversationEnd>>, BoundsError>| {
            composed.is_ok_and(|register| {
                register.map_or(true, |r| {
                    state
                        .clone()
                        .apply(r.hlc, SyncEntry::ConversationEnd(r.value))
                        == Ok(ApplyOutcome::Applied)
                })
            })
        };
        let mut late = SyncedState::new();
        late.apply_all(state.snapshot().stamped_entries());
        state.ended_conversations().iter().all(|e| {
            let conversation = &e.value.conversation;
            let raised = e.value.epoch.saturating_add(1);
            state.is_conversation_ended(conversation)
                && late.is_conversation_ended(conversation)
                && e.value.validate().is_ok()
                && takes_effect(state.compose_conversation_ended(0, conversation, raised))
                && takes_effect(state.compose_conversation_joined(0, conversation, raised))
        })
    }

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
    use crate::fuzz_api::{decode_synced_and_check, replay_and_check};

    #[test]
    fn the_synced_decode_oracle_holds_on_valid_and_damaged_input() {
        let mut state = crate::SyncedState::new();
        state.set_lineage(3, b"succ-a", b"root").unwrap();
        state.set_lineage(4, b"succ-b", b"root").unwrap();
        let envelope = state.lineage_envelopes().remove(0);
        state.set_recovery_email(5, Some("a@b.example")).unwrap();
        let email = state.recovery_email_envelope().unwrap();
        let persisted = state.to_bytes();
        let mut removed = state.clone();
        removed.set_recovery_email(6, None).unwrap();
        let removal = removed.recovery_email_envelope().unwrap();
        removed.set_own_username(7, Some("bob_42")).unwrap();
        let named = removed.own_username_envelope().unwrap();
        let persisted_removal = removed.to_bytes();
        removed.set_own_username(8, None).unwrap();
        let unnamed = removed.own_username_envelope().unwrap();
        let persisted_unnamed = removed.to_bytes();
        removed
            .set_link_record(
                9,
                crate::synced::LinkRecordEntry {
                    link_id: [4; 16],
                    link_secret: [5; 12],
                    kind: crate::synced::LinkRecordKind::Contact,
                    ledger_id: vec![6; 6],
                    revocation_seed: vec![8; 32],
                    expiry_ms: 0,
                    max_uses: 0,
                    uses: 0,
                    revoked: false,
                    shared: false,
                    minted_at_ms: 1,
                    label: "home".into(),
                    answered: vec![[7; 32]],
                },
            )
            .unwrap();
        let links = removed.link_record_envelopes().remove(0);
        let persisted_links = removed.to_bytes();
        for wire in [
            &envelope,
            &persisted,
            &email,
            &removal,
            &persisted_removal,
            &named,
            &unnamed,
            &persisted_unnamed,
            &links,
            &persisted_links,
        ] {
            assert!(decode_synced_and_check(wire));
            for i in 0..wire.len() {
                assert!(decode_synced_and_check(&wire[..i]));
                let mut flipped = wire.clone();
                flipped[i] ^= 0x41;
                assert!(decode_synced_and_check(&flipped));
            }
        }
        assert!(decode_synced_and_check(&[]));
    }

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
