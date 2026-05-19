// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group call participant ledger (Phase 3c1 scaffolding).
//!
//! Phase 3a shipped 1×1 P2P calls; Phase 3c extends to
//! group calls (up to ~50 participants per the plan). The
//! crypto primitives don't change — same MLS exporter
//! (`lakko/call/v1`), same per-participant SFrame key
//! derivation via [`crate::sframe::ParticipantKey`]. What
//! changes is who-is-on-the-call bookkeeping:
//!
//!   * 1×1 — two leaves in the conversation MLS group; both
//!     derive participant_key(0) and participant_key(1).
//!   * Group — N leaves (1 ≤ N ≤ 50). Each derives its own
//!     participant_key + every other leaf's key for the
//!     receive path. The ledger tracks which leaves are
//!     currently **on the call** (a subset of the MLS
//!     group's full membership — chat members may decline
//!     or never join a call).
//!
//! Two call shapes per ADR-0001:
//!
//!   * [`GroupCallScope::InChat`] — exporter from the
//!     existing conversation MLS group; the call_id
//!     identifies the call within that group.
//!   * [`GroupCallScope::LinkBased`] — fresh MLS group
//!     created when the call starts; callees join via a
//!     one-time KeyPackage embedded in the invite link.
//!     The ledger uses that fresh group as the keying
//!     substrate.
//!
//! ## Phase 3c1 scope
//!
//! This module ships the **ledger + key derivation** for a
//! group call. The signaling shape (who notifies whom about
//! join / leave on the wire) is the 3c2 follow-up; for now
//! the ledger consumes already-known `(leaf_index, joined?)`
//! events the caller produces.
//!
//! ## Rotation policy
//!
//! Per ADR-0001 + 0008, every participant Add / Remove on
//! the MLS group rotates the epoch; the next
//! [`GroupCallSession::refresh_call_key`] re-derives the
//! seed against the new epoch. The 1-second overlap window
//! (the old key remains accepted briefly so in-flight
//! frames decode) is a per-platform PeerConnection
//! concern — the ledger surfaces `needs_rekey()` after a
//! member change and the platform schedules the swap.

use std::collections::BTreeMap;

use lakko_mls::identity::Identity;
use lakko_mls::Group;

use crate::error::Result;
use crate::sframe::{CallKey, ParticipantKey};
use crate::signaling::{CallId, SignalingBody};

/// Which MLS group keys the call.
///
/// `InChat` keys off the existing conversation group; the
/// `call_id` identifies a specific call inside that group.
/// `LinkBased` keys off a fresh group spun up for the call.
///
/// The variants don't differ at the keying layer — both
/// reach an [`crate::sframe::CallKey`] via the same
/// exporter. The discriminator exists so call-side UI /
/// signaling code can know which audience the call is
/// scoped to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupCallScope {
    /// Call lives inside an existing conversation MLS group.
    /// Every chat member CAN join but isn't auto-joined.
    InChat,
    /// Call lives in a fresh MLS group created when the
    /// link was minted. Members joined via embedded one-time
    /// KeyPackages.
    LinkBased,
}

/// Per-participant state inside the ledger. The leaf index
/// is the receiver-side handle for participant_key
/// derivation; `joined` toggles when the participant enters
/// or leaves the call surface (distinct from their MLS
/// group membership).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Participant {
    /// MLS leaf index in the keying group.
    pub leaf_index: u32,
    /// `true` when the participant is currently on the
    /// call. The ledger doesn't auto-set this; call-side
    /// signaling drives the toggle via
    /// [`GroupCallSession::set_joined`].
    pub joined: bool,
}

/// In-memory ledger for a single group call.
///
/// Owns:
///   * The 32-byte [`CallKey`] seed derived from the MLS
///     exporter. Refreshed via
///     [`Self::refresh_call_key`] when the underlying MLS
///     group's epoch advances.
///   * The participant map — `leaf_index → Participant`.
///   * A `needs_rekey` flag the caller polls after every
///     state mutation; `true` means the platform should
///     re-derive every per-participant key + cycle the
///     SFrame transform on its `RTCPeerConnection`.
pub struct GroupCallSession {
    /// Scope this call is keyed against.
    pub scope: GroupCallScope,
    /// 16-byte random call_id. Same value across every
    /// participant; carried in every signaling event.
    pub call_id: CallId,
    /// Local participant's leaf index in the keying group.
    /// `participant_key(own_leaf_index)` is the SEAL side
    /// of every outbound frame.
    pub own_leaf_index: u32,
    /// 32-byte call-key seed. Re-derived from the MLS
    /// exporter at every epoch change.
    call_key: CallKey,
    /// Participant ledger. `BTreeMap` over `HashMap` so
    /// the iter order is stable for snapshot helpers + so
    /// integration tests can pattern-match the layout.
    participants: BTreeMap<u32, Participant>,
    /// Set to `true` on every membership change. The
    /// caller resets it after rekeying.
    needs_rekey: bool,
}

impl GroupCallSession {
    /// Create a fresh ledger from an existing MLS group +
    /// call metadata. The session opens with no
    /// participants joined; the caller invokes
    /// [`Self::set_joined`] for each remote leaf that
    /// signaled join, plus once for the local leaf (with
    /// `joined = true`).
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Exporter`] when the MLS exporter
    ///   rejects the label / length (practically
    ///   unreachable with valid inputs).
    pub fn create(
        scope: GroupCallScope,
        call_id: CallId,
        group: &Group,
        identity: &Identity,
    ) -> Result<Self> {
        let call_key = CallKey::derive(group, identity, &call_id)?;
        let own_leaf_index = group.own_leaf_index().u32();
        Ok(Self {
            scope,
            call_id,
            own_leaf_index,
            call_key,
            participants: BTreeMap::new(),
            needs_rekey: false,
        })
    }

    /// Re-derive the call-key seed from the MLS group.
    /// Caller invokes this after every Add / Remove commit
    /// on the keying group has been processed locally; the
    /// `needs_rekey` flag flips so the platform schedules
    /// the SFrame transform swap.
    ///
    /// Idempotent — calling twice at the same epoch
    /// produces the same seed. The platform's overlap
    /// window logic is on top.
    ///
    /// # Errors
    ///
    /// Surfaces the same `Error::Exporter` shapes as
    /// [`Self::create`].
    pub fn refresh_call_key(&mut self, group: &Group, identity: &Identity) -> Result<()> {
        self.call_key = CallKey::derive(group, identity, &self.call_id)?;
        self.own_leaf_index = group.own_leaf_index().u32();
        self.needs_rekey = true;
        Ok(())
    }

    /// Add (or update) a participant entry. `joined` is the
    /// "is on the call right now" toggle — distinct from
    /// the participant's MLS group membership. A `false`
    /// flips the entry to left-the-call but keeps the
    /// `leaf_index` so a re-join later picks up the same
    /// row.
    ///
    /// Calling for the local leaf is fine and recommended;
    /// the seal path looks up the own row to verify it's
    /// `joined`.
    pub fn set_joined(&mut self, leaf_index: u32, joined: bool) {
        let entry = self.participants.entry(leaf_index).or_insert(Participant {
            leaf_index,
            joined: false,
        });
        if entry.joined != joined {
            entry.joined = joined;
            // Membership-set change → schedule a re-key
            // sweep. Even if the platform's
            // PeerConnection is OK with the current keys,
            // we surface the option so a paranoid mode
            // can force the rotation.
            self.needs_rekey = true;
        }
    }

    /// Drop a participant entry entirely. Different from
    /// `set_joined(leaf, false)` — that keeps the row;
    /// this wipes it. Used when the MLS leaf is fully
    /// removed from the keying group.
    pub fn forget_participant(&mut self, leaf_index: u32) {
        if self.participants.remove(&leaf_index).is_some() {
            self.needs_rekey = true;
        }
    }

    /// Mark a remote leaf as joined the call. Convenience
    /// wrapper over [`Self::set_joined`] called from the
    /// signaling dispatcher when a [`SignalingBody::GroupJoin`]
    /// lands on the chat DS journal.
    pub fn apply_remote_join(&mut self, leaf_index: u32) {
        self.set_joined(leaf_index, true);
    }

    /// Mark a remote leaf as left the call. Mirrors
    /// [`Self::apply_remote_join`] for `GroupCallLeave`.
    /// Keeps the row (not `forget_participant`) so the UI
    /// can show "Alice left" without the leaf disappearing
    /// from the roster mid-render.
    pub fn apply_remote_leave(&mut self, leaf_index: u32) {
        self.set_joined(leaf_index, false);
    }

    /// Fold a remote signaling body into the ledger. Returns
    /// the [`SignalingOutcome`] the platform should react to:
    ///
    ///   * [`SignalingOutcome::Ignored`] — body wasn't a
    ///     group-control variant (i.e. an `Offer` / `Answer` /
    ///     `IceCandidate` / `End` from a 1×1 call leaked into
    ///     the dispatcher), or the body's `call_id` didn't
    ///     match this session.
    ///   * [`SignalingOutcome::Announced`] — `GroupCallStart`
    ///     accepted; the UI may render "Bob started a group
    ///     call". The ledger doesn't auto-join the initiator
    ///     — `GroupJoin` from the initiator's device lands
    ///     separately.
    ///   * [`SignalingOutcome::Joined { leaf_index }`] /
    ///     [`SignalingOutcome::Left { leaf_index }`] — roster
    ///     mutated; the platform consults `needs_rekey()` to
    ///     decide whether to cycle SFrame transforms.
    pub fn apply_remote_signaling(&mut self, body: &SignalingBody) -> SignalingOutcome {
        if body.call_id() != &self.call_id {
            return SignalingOutcome::Ignored;
        }
        match body {
            SignalingBody::GroupStart { .. } => SignalingOutcome::Announced,
            SignalingBody::GroupJoin { leaf_index, .. } => {
                self.apply_remote_join(*leaf_index);
                SignalingOutcome::Joined {
                    leaf_index: *leaf_index,
                }
            },
            SignalingBody::GroupLeave { leaf_index, .. } => {
                self.apply_remote_leave(*leaf_index);
                SignalingOutcome::Left {
                    leaf_index: *leaf_index,
                }
            },
            // Per-peer 1×1 bodies leaking into a group-call
            // dispatcher — typed away so the call-site
            // handles each surface separately. The ledger
            // doesn't react.
            SignalingBody::Offer { .. }
            | SignalingBody::Answer { .. }
            | SignalingBody::IceCandidate { .. }
            | SignalingBody::End { .. } => SignalingOutcome::Ignored,
        }
    }

    /// Snapshot of currently-joined participants. Stable
    /// ordering by leaf index — handy for UI projections
    /// + golden tests.
    #[must_use]
    pub fn joined_participants(&self) -> Vec<Participant> {
        self.participants
            .values()
            .filter(|p| p.joined)
            .copied()
            .collect()
    }

    /// Total participants seen on this call (joined +
    /// left). Useful for "X participants in total" UI
    /// strings.
    #[must_use]
    pub fn total_participants(&self) -> usize {
        self.participants.len()
    }

    /// Number of currently-joined participants. The local
    /// leaf is counted iff [`Self::set_joined`] has been
    /// called with `(own_leaf_index, true)`.
    #[must_use]
    pub fn joined_count(&self) -> usize {
        self.participants.values().filter(|p| p.joined).count()
    }

    /// Re-derive the local participant's SEAL key. Called
    /// after every `needs_rekey()` returns `true`.
    ///
    /// # Errors
    ///
    /// Surfaces the same `Error::Crypto` shapes as
    /// [`CallKey::participant_key`].
    pub fn own_participant_key(&self) -> Result<ParticipantKey> {
        self.call_key.participant_key(self.own_leaf_index)
    }

    /// Re-derive a remote participant's OPEN key. The
    /// platform side caches one per active leaf and feeds
    /// the corresponding key to its
    /// `RTCRtpReceiver.transform` insertable-streams hook.
    ///
    /// Returns the key regardless of whether the
    /// participant is currently joined — the caller may
    /// pre-warm keys before the join event lands. The
    /// `joined` filter is purely a UI / dispatch concern.
    ///
    /// # Errors
    ///
    /// See [`CallKey::participant_key`].
    pub fn remote_participant_key(&self, leaf_index: u32) -> Result<ParticipantKey> {
        self.call_key.participant_key(leaf_index)
    }

    /// `true` when the platform should re-derive every
    /// per-participant key + cycle the SFrame transform.
    /// Reset via [`Self::ack_rekey`] after the rotation
    /// completes.
    #[must_use]
    pub fn needs_rekey(&self) -> bool {
        self.needs_rekey
    }

    /// Mark the rekey sweep as complete. The caller
    /// invokes this after re-deriving every active
    /// participant's key + swapping the SFrame transform
    /// on the live `RTCPeerConnection`.
    pub fn ack_rekey(&mut self) {
        self.needs_rekey = false;
    }

    /// Borrow the underlying [`CallKey`] for advanced
    /// callers that want to derive keys for leaves not in
    /// the ledger (e.g. a passive observer mode).
    #[must_use]
    pub fn call_key(&self) -> &CallKey {
        &self.call_key
    }
}

/// Result of folding a remote [`SignalingBody`] into the
/// ledger via [`GroupCallSession::apply_remote_signaling`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalingOutcome {
    /// Body wasn't applicable to this session — either a
    /// 1×1 variant leaked into the group dispatcher, or the
    /// `call_id` didn't match. Ledger unchanged.
    Ignored,
    /// Group call was announced. UI may render a "Bob
    /// started a call" affordance; the ledger doesn't
    /// auto-join anyone — the initiator's own
    /// `GroupCallJoin` lands separately.
    Announced,
    /// Remote leaf joined the call. `leaf_index` is the
    /// MLS leaf that flipped to `joined = true`.
    Joined {
        /// MLS leaf index of the joining participant.
        leaf_index: u32,
    },
    /// Remote leaf left the call. `leaf_index` is the MLS
    /// leaf that flipped to `joined = false`.
    Left {
        /// MLS leaf index of the leaving participant.
        leaf_index: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_mls::Identity as MlsIdentity;

    fn make_pair() -> (Identity, Identity, Group) {
        let id_a = MlsIdentity::generate("device-A").unwrap();
        let id_b = MlsIdentity::generate("device-B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let mut group_a = lakko_mls::Group::create(&id_a).unwrap();
        let addition = group_a.add_member(&id_a, kp_b).unwrap();
        let _welcome_bytes = lakko_mls::group::encode_message(&addition.welcome).unwrap();
        (id_a, id_b, group_a)
    }

    fn fixture_call_id() -> CallId {
        let mut id = [0u8; 16];
        id[0] = 0x10;
        id[15] = 0x10;
        id
    }

    #[test]
    fn create_seeds_call_key() {
        let (id_a, _id_b, group_a) = make_pair();
        let session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();
        assert_eq!(session.scope, GroupCallScope::InChat);
        assert_eq!(session.call_id, fixture_call_id());
        assert_eq!(session.own_leaf_index, 0);
        assert_eq!(session.total_participants(), 0);
        assert_eq!(session.joined_count(), 0);
        assert!(!session.needs_rekey());
    }

    #[test]
    fn set_joined_toggles_count_and_rekey() {
        let (id_a, _id_b, group_a) = make_pair();
        let mut session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();

        session.set_joined(0, true);
        assert_eq!(session.joined_count(), 1);
        assert!(session.needs_rekey());
        session.ack_rekey();
        assert!(!session.needs_rekey());

        // Idempotent — same toggle, no rekey.
        session.set_joined(0, true);
        assert!(!session.needs_rekey());

        // Drop the local leaf.
        session.set_joined(0, false);
        assert_eq!(session.joined_count(), 0);
        assert!(session.needs_rekey());
    }

    #[test]
    fn forget_participant_wipes_row() {
        let (id_a, _id_b, group_a) = make_pair();
        let mut session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();
        session.set_joined(0, true);
        session.set_joined(1, true);
        session.ack_rekey();
        assert_eq!(session.total_participants(), 2);

        session.forget_participant(1);
        assert_eq!(session.total_participants(), 1);
        assert!(session.needs_rekey());
    }

    #[test]
    fn participant_keys_differ_across_leaves() {
        let (id_a, _id_b, group_a) = make_pair();
        let session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();
        let k_self = session.own_participant_key().unwrap();
        let k_remote = session.remote_participant_key(1).unwrap();
        assert_ne!(k_self.key_bytes(), k_remote.key_bytes());
        assert_ne!(k_self.nonce_salt(), k_remote.nonce_salt());
    }

    #[test]
    fn refresh_call_key_picks_up_new_epoch() {
        // Advance the MLS group's epoch by adding a third
        // member. The refreshed call_key should be
        // different (the exporter folds the epoch into its
        // derivation).
        let id_a = MlsIdentity::generate("device-A").unwrap();
        let id_b = MlsIdentity::generate("device-B").unwrap();
        let id_c = MlsIdentity::generate("device-C").unwrap();
        let mut group_a = lakko_mls::Group::create(&id_a).unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let _ = group_a.add_member(&id_a, kp_b).unwrap();

        let mut session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();
        let pre = *session.call_key().as_bytes();

        let kp_c = id_c.create_key_package().unwrap();
        let _ = group_a.add_member(&id_a, kp_c).unwrap();
        session.refresh_call_key(&group_a, &id_a).unwrap();
        let post = *session.call_key().as_bytes();

        assert_ne!(pre, post, "epoch advance must rotate the call-key seed");
        assert!(session.needs_rekey());
    }

    #[test]
    fn apply_remote_signaling_routes_each_group_body() {
        use crate::signaling::{EndReason, MediaFlags, SignalingBody};

        let (id_a, _id_b, group_a) = make_pair();
        let mut session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();

        // Announce — ledger unchanged, returns Announced.
        let outcome = session.apply_remote_signaling(&SignalingBody::GroupStart {
            call_id: fixture_call_id(),
            initiator_leaf: 0,
            created_at_ms: 1,
            media: MediaFlags::audio_only(),
        });
        assert_eq!(outcome, SignalingOutcome::Announced);
        assert_eq!(session.joined_count(), 0);
        assert!(!session.needs_rekey());

        // Join — leaf 1 flips to joined, returns Joined.
        let outcome = session.apply_remote_signaling(&SignalingBody::GroupJoin {
            call_id: fixture_call_id(),
            leaf_index: 1,
            joined_at_ms: 2,
        });
        assert_eq!(outcome, SignalingOutcome::Joined { leaf_index: 1 });
        assert_eq!(session.joined_count(), 1);
        assert!(session.needs_rekey());
        session.ack_rekey();

        // Leave — leaf 1 flips back, returns Left.
        let outcome = session.apply_remote_signaling(&SignalingBody::GroupLeave {
            call_id: fixture_call_id(),
            leaf_index: 1,
            reason: EndReason::HungUp,
        });
        assert_eq!(outcome, SignalingOutcome::Left { leaf_index: 1 });
        assert_eq!(session.joined_count(), 0);
        assert!(session.needs_rekey());
    }

    #[test]
    fn apply_remote_signaling_ignores_one_to_one_bodies() {
        use crate::signaling::{EndReason, MediaFlags, SignalingBody};

        let (id_a, _id_b, group_a) = make_pair();
        let mut session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();

        for body in [
            SignalingBody::Offer {
                call_id: fixture_call_id(),
                sdp: "v=0\n".into(),
                created_at_ms: 0,
                media: MediaFlags::audio_only(),
            },
            SignalingBody::Answer {
                call_id: fixture_call_id(),
                sdp: "v=0\n".into(),
            },
            SignalingBody::IceCandidate {
                call_id: fixture_call_id(),
                candidate: String::new(),
                sdp_mid: "audio".into(),
                sdp_m_line_index: 0,
            },
            SignalingBody::End {
                call_id: fixture_call_id(),
                reason: EndReason::HungUp,
            },
        ] {
            let outcome = session.apply_remote_signaling(&body);
            assert_eq!(outcome, SignalingOutcome::Ignored, "{body:?}");
        }
        assert_eq!(session.joined_count(), 0);
        assert!(!session.needs_rekey());
    }

    #[test]
    fn apply_remote_signaling_ignores_wrong_call_id() {
        use crate::signaling::SignalingBody;

        let (id_a, _id_b, group_a) = make_pair();
        let mut session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();
        let mut other = fixture_call_id();
        other[0] = 0xFF;
        let outcome = session.apply_remote_signaling(&SignalingBody::GroupJoin {
            call_id: other,
            leaf_index: 1,
            joined_at_ms: 0,
        });
        assert_eq!(outcome, SignalingOutcome::Ignored);
        assert_eq!(session.joined_count(), 0);
        assert!(!session.needs_rekey());
    }

    #[test]
    fn joined_participants_iter_is_stable() {
        let (id_a, _id_b, group_a) = make_pair();
        let mut session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();
        // Insert out of order — BTreeMap should sort.
        session.set_joined(7, true);
        session.set_joined(3, true);
        session.set_joined(5, true);
        let joined = session.joined_participants();
        let indices: Vec<u32> = joined.iter().map(|p| p.leaf_index).collect();
        assert_eq!(indices, vec![3, 5, 7]);
    }
}
