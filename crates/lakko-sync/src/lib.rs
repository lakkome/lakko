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
    /// * `decode_quick_reaction_envelope` — the account's quick reaction (UX
    ///   reactions-and-emoji D3), reached the same way, held to the same
    ///   properties.
    /// * [`decode_link_record_envelope`] — the account's invite links
    ///   (ADR-0079), reached the same way. Whatever decodes must re-encode to
    ///   the identical bytes, hold records within their bounds, and fold so
    ///   that every revoked record it carries is held revoked.
    /// * `decode_channel_admission_seed_envelope` — a private channel's
    ///   admission seeds (ADR-0090), reached the same way: canonical, folding
    ///   to exactly its registers (two seeds of one version coexist),
    ///   write-once per seed at the writer.
    /// * `decode_device_ack_envelope` — the device-acknowledgement registers
    ///   (identity-recovery 17.5), reached the same way: canonical, folding
    ///   to exactly its keys, not applying twice.
    /// * `decode_blocked_envelope` — the block registers (ADR-0098 section
    ///   6), reached the same way: canonical, folding to exactly its keys and
    ///   states, not applying twice.
    /// * `decode_poll_choice_envelope` — the account's own poll choices
    ///   (ADR-0092 section 3.6), reached the same way: canonical, within its
    ///   bounds, folding to exactly its registers, carried back by
    ///   `poll_choice_envelopes`, not applying twice, and pruned once every
    ///   conversation it names has ended.
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
            && quick_reaction_envelope_holds(bytes)
            && link_record_envelope_holds(bytes)
            && device_ack_envelope_holds(bytes)
            && blocked_envelope_holds(bytes)
            && channel_reaction_envelope_holds(bytes)
            && channel_comment_envelope_holds(bytes)
            && channel_subscription_envelope_holds(bytes)
            && channel_admission_seed_envelope_holds(bytes)
            && poll_choice_envelope_holds(bytes)
            && persisted_state_holds(bytes)
    }

    /// The channel-subscription envelope (2026-11-13): canonical, within its
    /// bounds, folds to exactly its registers, is carried back by
    /// `channel_subscription_envelopes`, does not apply twice, a live
    /// register it carries reads as subscribed and a leave as not, and an
    /// owner key is carried exactly by a co-publisher (ADR-0090 2.3).
    fn channel_subscription_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(entries) = crate::synced::decode_channel_subscription_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        crate::synced::compose_channel_subscription_envelope(&entries).as_deref() == Ok(bytes)
            && bytes.len() <= crate::synced::CHANNEL_SUBSCRIPTION_ENVELOPE_MAX_LEN
            && entries.iter().all(|e| e.value.validate().is_ok())
            && state.apply_channel_subscription_envelope(bytes) == Ok(true)
            && state.all_channel_subscriptions() == entries
            && state.channel_subscription_envelopes() == vec![bytes.to_vec()]
            && state.apply_channel_subscription_envelope(bytes) == Ok(false)
            && entries.iter().all(|e| {
                state.is_channel_subscribed(&e.value.channel_id) == e.value.is_subscribed()
                    && e.value.subscribed.as_ref().map_or(true, |s| {
                        (s.role == crate::synced::ChannelSubscriptionRole::CoPublisher)
                            == s.owner_account_key.is_some()
                    })
            })
    }

    /// The channel admission-seed envelope (ADR-0090 open question 4):
    /// canonical, within its bounds, folds to exactly its registers, is
    /// carried back by `channel_admission_seed_envelopes`, does not apply
    /// twice, every seed reads back among the seeds of its version, and a
    /// local write of a held seed writes nothing.
    fn channel_admission_seed_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(entries) = crate::synced::decode_channel_admission_seed_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        crate::synced::compose_channel_admission_seed_envelope(&entries).as_deref() == Ok(bytes)
            && bytes.len() <= crate::synced::CHANNEL_ADMISSION_SEED_ENVELOPE_MAX_LEN
            && state.apply_channel_admission_seed_envelope(bytes) == Ok(true)
            && state.all_channel_admission_seeds() == entries
            && state.channel_admission_seed_envelopes() == vec![bytes.to_vec()]
            && state.apply_channel_admission_seed_envelope(bytes) == Ok(false)
            && entries.iter().all(|e| {
                state
                    .channel_admission_seeds_for(&e.value.channel_id, e.value.admission_version)
                    .contains(&e.value.seed)
                    && !state.set_channel_admission_seed(
                        u64::MAX,
                        &e.value.channel_id,
                        e.value.admission_version,
                        &e.value.seed,
                    )
                    && state
                        .channel_admission_seed_envelope_for(
                            &e.value.channel_id,
                            e.value.admission_version,
                            &e.value.seed,
                        )
                        .is_some()
            })
    }

    /// The own channel-reaction envelope (ADR-0087 section 13a; a reaction on
    /// a post or, since phase B, on one of its comments): canonical,
    /// within its bounds, folds to exactly its registers, is carried back by
    /// `channel_reaction_envelopes`, does not apply twice, and a prune far
    /// past every post leaves none of it.
    fn channel_reaction_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(entries) = crate::synced::decode_channel_reaction_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        let held = crate::synced::compose_channel_reaction_envelope(&entries).as_deref()
            == Ok(bytes)
            && bytes.len() <= crate::synced::CHANNEL_REACTION_ENVELOPE_MAX_LEN
            && entries.iter().all(|e| e.value.validate().is_ok())
            && state.apply_channel_reaction_envelope(bytes) == Ok(true)
            && state.channel_reaction_envelopes() == vec![bytes.to_vec()]
            && state.apply_channel_reaction_envelope(bytes) == Ok(false)
            && entries.iter().all(|e| {
                let v = &e.value;
                let stands = match v.comment_cursor {
                    None => state.has_channel_reaction(&v.channel_id, v.post_cursor, &v.emoji),
                    Some(c) => {
                        state.has_comment_reaction(&v.channel_id, v.post_cursor, c, &v.emoji)
                    },
                };
                stands == v.reacted
            });
        held && {
            let n = state.len();
            state.prune_channel_activity(u64::MAX) == n && state.is_empty()
        }
    }

    /// The own poll-choice envelope (ADR-0092 section 3.6): canonical,
    /// within its bounds, folds to exactly its registers, is carried back by
    /// `poll_choice_envelopes`, does not apply twice, survives a prune while
    /// no conversation has ended, and is gone after every conversation it
    /// names has ended.
    fn poll_choice_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(entries) = crate::synced::decode_poll_choice_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        let held = crate::synced::compose_poll_choice_envelope(&entries).as_deref() == Ok(bytes)
            && bytes.len() <= crate::synced::POLL_CHOICE_ENVELOPE_MAX_LEN
            && entries.iter().all(|e| e.value.validate().is_ok())
            && state.apply_poll_choice_envelope(bytes) == Ok(true)
            && state.poll_choice_envelopes() == vec![bytes.to_vec()]
            && state.apply_poll_choice_envelope(bytes) == Ok(false)
            && entries.iter().all(|e| {
                state
                    .poll_choice(&e.value.conversation, e.value.poll_message_id)
                    .as_ref()
                    == Some(e)
            })
            && state.prune_poll_choices() == 0;
        held && {
            let n = state.len();
            for e in &entries {
                if state
                    .apply(
                        u64::MAX,
                        SyncEntry::ConversationEnd(ConversationEnd {
                            conversation: e.value.conversation.clone(),
                            state: crate::synced::entry::ConversationEndState::Ended,
                            epoch: u64::MAX,
                        }),
                    )
                    .is_err()
                {
                    return false;
                }
            }
            state.prune_poll_choices() == n
                && entries.iter().all(|e| {
                    state
                        .poll_choice(&e.value.conversation, e.value.poll_message_id)
                        .is_none()
                })
        }
    }

    /// The own-comments envelope (ADR-0087 section 13a): canonical, folds to
    /// exactly its registers, is carried back by `channel_comment_envelopes`,
    /// does not apply twice, lists exactly its live nonces, and a prune past
    /// every post removes it all.
    fn channel_comment_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(entries) = crate::synced::decode_channel_comment_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        let held = crate::synced::compose_channel_comment_envelope(&entries).as_deref()
            == Ok(bytes)
            && bytes.len() <= crate::synced::CHANNEL_COMMENT_ENVELOPE_MAX_LEN
            && state.apply_channel_comment_envelope(bytes) == Ok(true)
            && state.channel_comment_envelopes() == vec![bytes.to_vec()]
            && state.apply_channel_comment_envelope(bytes) == Ok(false)
            && entries.iter().all(|e| {
                state
                    .channel_comments_on(&e.value.channel_id, e.value.post_cursor)
                    .contains(&e.value.comment_nonce)
                    != e.value.deleted
            });
        held && {
            let n = state.len();
            state.prune_channel_activity(u64::MAX) == n && state.is_empty()
        }
    }

    /// The block envelope (ADR-0098 section 6): canonical, folds to exactly
    /// its keys and states, carried back by `blocked_envelopes`, and does
    /// not apply twice.
    fn blocked_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(entries) = crate::synced::decode_blocked_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        crate::synced::compose_blocked_envelope(&entries).as_deref() == Ok(bytes)
            && state.apply_blocked_envelope(bytes) == Ok(true)
            && entries
                .iter()
                .all(|e| state.is_blocked(&e.value.account_key) == e.value.blocked)
            && state.blocked_envelopes() == vec![bytes.to_vec()]
            && state.apply_blocked_envelope(bytes) == Ok(false)
    }

    /// The device-acknowledgement envelope (identity-recovery 17.5):
    /// canonical, folds to exactly its keys, carried back by
    /// `device_ack_envelopes`, and does not apply twice.
    fn device_ack_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(entries) = crate::synced::decode_device_ack_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        let keys: Vec<[u8; 32]> = entries.iter().map(|e| e.value.device_key).collect();
        crate::synced::compose_device_ack_envelope(&entries).as_deref() == Ok(bytes)
            && state.apply_device_ack_envelope(bytes) == Ok(true)
            && state.acknowledged_devices() == keys
            && state.device_ack_envelopes() == vec![bytes.to_vec()]
            && state.apply_device_ack_envelope(bytes) == Ok(false)
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
            // A group or channel link (ADR-0087 phase B) never accepts a
            // contact request, even under its own secret.
            && entries.iter().all(|e| {
                !e.value.kind.is_group_link()
                    || e.value.verdict(&e.value.link_secret, &[0; 32], 0)
                        != crate::synced::LinkAnswerVerdict::Accept
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

    /// The quick-reaction envelope (UX reactions-and-emoji D3): canonical,
    /// within its bounds, folds to exactly its own register, and a re-send is
    /// not an update.
    fn quick_reaction_envelope_holds(bytes: &[u8]) -> bool {
        let Ok(register) = crate::synced::decode_quick_reaction_envelope(bytes) else {
            return true;
        };
        let mut state = SyncedState::new();
        crate::synced::compose_quick_reaction_envelope(&register).as_deref() == Ok(bytes)
            && register.value.validate().is_ok()
            && bytes.len() <= crate::synced::QUICK_REACTION_ENVELOPE_MAX_LEN
            && state.apply_quick_reaction_envelope(bytes) == Ok(true)
            && state.quick_reaction_register().as_ref() == Some(&register)
            && state.quick_reaction_envelope().as_deref() == Some(bytes)
            && state.apply_quick_reaction_envelope(bytes) == Ok(false)
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
        if let Some(envelope) = state.quick_reaction_envelope() {
            if rebuilt.apply_quick_reaction_envelope(&envelope).is_err() {
                return false;
            }
        }
        for envelope in state.link_record_envelopes() {
            if rebuilt.apply_link_record_envelope(&envelope).is_err() {
                return false;
            }
        }
        for envelope in state.channel_reaction_envelopes() {
            if rebuilt.apply_channel_reaction_envelope(&envelope).is_err() {
                return false;
            }
        }
        for envelope in state.channel_comment_envelopes() {
            if rebuilt.apply_channel_comment_envelope(&envelope).is_err() {
                return false;
            }
        }
        for envelope in state.channel_subscription_envelopes() {
            if rebuilt
                .apply_channel_subscription_envelope(&envelope)
                .is_err()
            {
                return false;
            }
        }
        for envelope in state.channel_admission_seed_envelopes() {
            if rebuilt
                .apply_channel_admission_seed_envelope(&envelope)
                .is_err()
            {
                return false;
            }
        }
        for envelope in state.poll_choice_envelopes() {
            if rebuilt.apply_poll_choice_envelope(&envelope).is_err() {
                return false;
            }
        }
        let reactions = |s: &SyncedState| {
            s.entries()
                .into_iter()
                .filter(|(_, e)| {
                    matches!(
                        e,
                        SyncEntry::ChannelReaction(_)
                            | SyncEntry::ChannelComment(_)
                            | SyncEntry::PollChoice(_)
                    )
                })
                .collect::<Vec<_>>()
        };
        if reactions(&rebuilt) != reactions(&state) {
            return false;
        }
        rebuilt.all_lineage() == state.all_lineage()
            && rebuilt.all_link_records() == state.all_link_records()
            && rebuilt.all_channel_subscriptions() == state.all_channel_subscriptions()
            && rebuilt.all_channel_admission_seeds() == state.all_channel_admission_seeds()
            && rebuilt.recovery_email_register() == state.recovery_email_register()
            && rebuilt.own_username_register() == state.own_username_register()
            && rebuilt.quick_reaction_register() == state.quick_reaction_register()
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
    fn the_synced_decode_oracle_holds_on_poll_choices() {
        let mut state = crate::SyncedState::new();
        let group = crate::synced::ConversationRef {
            kind: crate::synced::entry::ConversationRefKind::Group,
            id: vec![1; 32],
        };
        let channel = crate::synced::ConversationRef {
            kind: crate::synced::entry::ConversationRefKind::Channel,
            id: vec![2; 16],
        };
        let live = state
            .set_poll_choice(3, &group, 9, &[0, 4], 7)
            .unwrap()
            .unwrap();
        state
            .set_poll_choice(4, &channel, 120, &[], 7)
            .unwrap()
            .unwrap();
        assert!(decode_synced_and_check(&live));
        for envelope in state.poll_choice_envelopes() {
            assert!(decode_synced_and_check(&envelope));
            let mut damaged = envelope.clone();
            if let Some(last) = damaged.last_mut() {
                *last ^= 0xFF;
            }
            assert!(decode_synced_and_check(&damaged));
        }
        assert!(decode_synced_and_check(&state.to_bytes()));
    }

    #[test]
    // One wire per register kind, each damaged every way; splitting it would
    // only move the list.
    #[allow(clippy::too_many_lines)]
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
                    target_id: vec![],
                    invite_seed: vec![],
                    admission_version: 0,
                    capability: vec![],
                },
            )
            .unwrap();
        let links = removed.link_record_envelopes().remove(0);
        let persisted_links = removed.to_bytes();
        let reacted = removed
            .set_channel_reaction(10, [3; 16], 7, "\u{1F44D}", true, 1_000)
            .unwrap()
            .unwrap();
        let unreacted = removed
            .set_channel_reaction(11, [3; 16], 7, "\u{1F44D}", false, 1_000)
            .unwrap()
            .unwrap();
        let persisted_reactions = removed.to_bytes();
        let commented = removed
            .record_channel_comment(12, [3; 16], 7, [9; 16], 1_000)
            .unwrap();
        let uncommented = removed
            .delete_channel_comment(13, [3; 16], 7, [9; 16], 1_000)
            .unwrap();
        let persisted_comments = removed.to_bytes();
        removed
            .set_channel_subscription(
                14,
                &[5; 16],
                crate::synced::ChannelSubscribed {
                    role: crate::synced::ChannelSubscriptionRole::CoPublisher,
                    subscriber_secret: [6; 32],
                    read_key: Some([7; 32]),
                    publisher_group_id: Some([8; 32]),
                    owner_account_key: Some([9; 32]),
                    name: "News".into(),
                    handle: "news".into(),
                },
            )
            .unwrap();
        let subscription = removed.channel_subscription_envelopes().remove(0);
        assert!(removed.leave_channel_subscription(15, &[5; 16]));
        let left = removed.channel_subscription_envelopes().remove(0);
        let persisted_subscriptions = removed.to_bytes();
        let blocked = removed.set_blocked(16, [4; 32], true, 1_000).unwrap();
        let unblocked = removed.set_blocked(17, [4; 32], false, 2_000).unwrap();
        let persisted_blocked = removed.to_bytes();
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
            &reacted,
            &unreacted,
            &persisted_reactions,
            &commented,
            &uncommented,
            &persisted_comments,
            &subscription,
            &left,
            &persisted_subscriptions,
            &blocked,
            &unblocked,
            &persisted_blocked,
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
