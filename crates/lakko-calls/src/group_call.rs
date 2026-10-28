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
//!     derive participant keys per `(leaf, stream)` KID.
//!   * Group — N leaves (1 ≤ N ≤ 50). Each derives its own
//!     participant_key per stream + every other leaf's key
//!     for the receive path (per stream — the ADR-0035 KID
//!     `(leaf << 8) | stream_index` splits a leaf's concurrent
//!     streams). The ledger tracks which leaves are
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
//! ## Scope
//!
//! This module owns the **ledger + key derivation** for a
//! group call. The signaling wire shape (who notifies whom
//! about join / leave) lives in [`crate::signaling`]
//! ([`SignalingBody`]); the ledger folds those events in via
//! [`GroupCallSession::apply_remote_signaling`] (and the
//! lower-level [`GroupCallSession::apply_remote_join`] /
//! [`GroupCallSession::apply_remote_leave`]). The caller may
//! also drive the `(leaf_index, joined?)` toggle directly
//! through [`GroupCallSession::set_joined`].
//!
//! ## Rotation policy (Phase 3c5)
//!
//! Per ADR-0001 + 0008, every participant Add / Remove on
//! the MLS group rotates the epoch; the next
//! [`GroupCallSession::refresh_call_key`] re-derives the
//! seed against the new epoch. The previous seed is kept
//! in [`GroupCallSession::previous_call_key`] for the
//! **1-second overlap window** — receive-side
//! [`GroupCallSession::try_open_with_overlap`] first tries
//! the current seed's per-participant key, then falls back
//! to the previous seed's key if the new one rejects. This
//! absorbs in-flight frames sealed under the OLD key that
//! land after the rotation (RTT, jitter, reorder).
//!
//! The crate does **not** read a wall-clock — but the
//! expiry **decision** lives here, not in three per-platform
//! timers (G-18). [`GroupCallSession::refresh_call_key_at`]
//! stamps the retained previous key with `now_ms + clamp(
//! overlap_ms)` (default [`crate::OVERLAP_DEFAULT_MS`] ≈ 1 s), and
//! [`GroupCallSession::prune_expired_previous`] drops it once
//! the caller's clock passes that instant — so the platform
//! only supplies `now`, while the bounds + the 1-second policy
//! are owned + tested in the core. The legacy
//! [`GroupCallSession::refresh_call_key`] (no clock) +
//! [`GroupCallSession::drop_previous_call_key`] (manual drop)
//! remain for callers that drive their own timer. Either way,
//! once the previous key is dropped, frames under the old key
//! cease to decode and seed-boundary forward secrecy is fully
//! in effect.

use std::collections::BTreeMap;

use lakko_mls::identity::Identity;
use lakko_mls::Group;

use crate::call_key_ring::{clamp_overlap, CallKeyRing};
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
    /// The overlap-window call-key ring — the current [`CallKey`] plus, during
    /// a rotation, the previous one retained for the 1-second overlap window.
    /// The overlap logic (retain-until-expiry, clamp, try-current-then-previous)
    /// lives once in [`CallKeyRing`] and is shared with the FFI-exposed client
    /// media layers (G-18, ADR-0029).
    ring: CallKeyRing,
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
            ring: CallKeyRing::new(call_key),
            participants: BTreeMap::new(),
            needs_rekey: false,
        })
    }

    /// Re-derive the call-key seed from the MLS group +
    /// stash the prior seed into
    /// [`Self::previous_call_key`] for the 1-second overlap
    /// window (Phase 3c5). Caller invokes this after every
    /// Add / Remove commit on the keying group has been
    /// processed locally; the `needs_rekey` flag flips so
    /// the platform schedules the SFrame transform swap.
    ///
    /// **Idempotent across the same epoch — a same-seed
    /// refresh is a no-op.** Calling at the same epoch
    /// re-derives the identical seed; the core **refuses to
    /// rotate** in that case — no previous key is stashed and
    /// `needs_rekey` stays unset, so the platform is not
    /// prompted to swap its seal key when nothing rotated. The
    /// guard is defence in depth: since ADR-0069 a key from
    /// [`Self::own_participant_key`] starts at a random counter
    /// in `[0, 2^62)`, so even a seal key derived again under
    /// an unchanged seed reuses a `(key, nonce)` pair only with
    /// probability at most `(L1 + L2) / 2^62` for `L1` and `L2`
    /// frames sealed; the guard is no longer the only thing
    /// between a spurious refresh and a reused nonce. Only
    /// a genuine epoch advance (a distinct exporter seed)
    /// rotates. Calling across N epochs in rapid succession
    /// only retains the **most recent previous** epoch's seed;
    /// older overlap windows fall off. The 1-second overlap
    /// budget assumes ≤ 1 commit per second per call, which
    /// matches the SFU's commit debouncing.
    ///
    /// # Errors
    ///
    /// Surfaces the same `Error::Exporter` shapes as
    /// [`Self::create`].
    pub fn refresh_call_key(&mut self, group: &Group, identity: &Identity) -> Result<()> {
        // No clock supplied → the previous key never auto-expires; the caller
        // must invoke `drop_previous_call_key`. Preserved for backward compat;
        // prefer `refresh_call_key_at` so the core owns the expiry (G-18).
        self.rotate_call_key(group, identity, u64::MAX)
    }

    /// Clock-aware variant of [`Self::refresh_call_key`] (G-18). Retains the
    /// previous-epoch key until `now_ms + clamp(overlap_ms)`, so the 1-second
    /// overlap **decision** lives in the core rather than in three per-platform
    /// timers. `overlap_ms` is clamped to `[OVERLAP_MIN_MS, OVERLAP_MAX_MS]`
    /// (pass [`crate::OVERLAP_DEFAULT_MS`] for the ADR-0008 default). The caller still
    /// owns the wall clock — the core stays time-source-free.
    ///
    /// # Errors
    ///
    /// Surfaces the same `Error::Exporter` shapes as [`Self::create`].
    pub fn refresh_call_key_at(
        &mut self,
        group: &Group,
        identity: &Identity,
        now_ms: u64,
        overlap_ms: u64,
    ) -> Result<()> {
        let expires_at_ms = now_ms.saturating_add(clamp_overlap(overlap_ms));
        self.rotate_call_key(group, identity, expires_at_ms)
    }

    fn rotate_call_key(
        &mut self,
        group: &Group,
        identity: &Identity,
        expires_at_ms: u64,
    ) -> Result<()> {
        let next = CallKey::derive(group, identity, &self.call_id)?;
        // Defence in depth: a same-seed refresh (no MLS epoch advanced) is a
        // total no-op. `CallKeyRing::rekey` already refuses to stash a previous
        // key for an unchanged seed; suppressing `needs_rekey` here is its
        // companion — nothing rotated, so the platform is not prompted to derive
        // a new seal key. Were it to derive one anyway, that key starts at its
        // own random counter (ADR-0069), so this guard is no longer what keeps a
        // nonce from being reused. Only a genuine rotation (a distinct seed)
        // touches the ledger. Constant-time seed compare (`CallKey::seed_eq`).
        if self.ring.current().seed_eq(&next) {
            return Ok(());
        }
        // C-10: the ring overwrites any earlier stash — only the most-recent
        // previous epoch is retained.
        self.ring.rekey(next, expires_at_ms);
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
            | SignalingBody::End { .. }
            // A one-to-one share state (ADR-0083) is the client's
            // `screen_share` fold, not the roster's.
            | SignalingBody::ScreenShare { .. } => SignalingOutcome::Ignored,
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

    /// Re-derive the local participant's SEAL key for one of its outbound
    /// streams. Called after every `needs_rekey()` returns `true`. The sender
    /// assigns `stream_index` in a stable local order (0 for the first stream,
    /// 1..N for additional ones — e.g. camera + screen-share); folding the
    /// resulting KID `(own_leaf << 8) | stream_index` into the derivation
    /// (ADR-0035) keeps each of the local participant's streams on a distinct
    /// key + nonce-salt space so their independent counters cannot reuse a
    /// `(key, nonce)` pair. The returned key starts at a uniformly random
    /// counter in `[0, 2^62)` (ADR-0069), so deriving it again under an
    /// unchanged seed is safe; see [`CallKey::participant_key`] for the bound.
    ///
    /// # Errors
    ///
    /// - `Error::Crypto` when `stream_index` does not fit the KID's 8-bit stream
    ///   field, or on an HKDF failure (practically unreachable). See
    ///   [`CallKey::participant_key_for`].
    pub fn own_participant_key(&self, stream_index: u32) -> Result<ParticipantKey> {
        self.ring
            .current()
            .participant_key_for(self.own_leaf_index, stream_index)
    }

    /// Re-derive a remote sender's OPEN key from the composite `kid` the SFrame
    /// header carries (ADR-0035: `(sender_leaf << 8) | stream_index`). The
    /// platform side caches one key per active `(leaf, stream)` and feeds it to
    /// its `RTCRtpReceiver.transform` insertable-streams hook.
    ///
    /// The receiver needs NO local media-type or track-kind signal — the KID it
    /// read from the header (forwarded verbatim by the SFU) IS the discriminator.
    /// Recover the leaf/stream for roster/diagnostic use with
    /// [`SframeHeader::leaf_of`](crate::header::SframeHeader::leaf_of) /
    /// [`SframeHeader::stream_of`](crate::header::SframeHeader::stream_of).
    ///
    /// Returns the key regardless of whether the participant is currently joined
    /// — the caller may pre-warm keys before the join event lands. The `joined`
    /// filter is purely a UI / dispatch concern.
    ///
    /// # Errors
    ///
    /// See [`CallKey::participant_key`].
    pub fn remote_participant_key(&self, kid: u64) -> Result<ParticipantKey> {
        self.ring.participant_key(kid)
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
        self.ring.current()
    }

    /// Borrow the previous-epoch seed retained for the
    /// overlap window, if one is currently held. Returns
    /// `None` outside an overlap window (initial state +
    /// after [`Self::drop_previous_call_key`]).
    ///
    /// Diagnostic / test surface — production callers
    /// should funnel through [`Self::try_open_with_overlap`]
    /// rather than re-deriving from this directly.
    #[must_use]
    pub fn previous_call_key(&self) -> Option<&CallKey> {
        self.ring.previous()
    }

    /// Open a received SFrame with the 1-second overlap
    /// behaviour: try the current seed's per-stream
    /// key for `kid` (ADR-0035) first; on AEAD failure (the
    /// frame was sealed under the OLD seed but landed late),
    /// fall back to the previous seed's key. Returns the
    /// plaintext on either path.
    ///
    /// Outside an overlap window — i.e. when no rotation
    /// has happened yet, or after the platform invoked
    /// [`Self::drop_previous_call_key`] — this collapses to
    /// "current key only" and the second path is skipped.
    ///
    /// `frame_counter` is the explicit 64-bit counter the SFrame
    /// header carries (RFC 9605 §4.3, ADR-0069); the producer's
    /// monotonic seal-side counter survives the rotation
    /// because the receiver doesn't need to know which
    /// epoch produced a frame — just whether the AEAD
    /// authenticates.
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Crypto`] when the frame fails to
    ///   authenticate under **both** the current and (if
    ///   present) previous keys. This is the same shape
    ///   [`ParticipantKey::open`] returns — the overlap is
    ///   transparent to the error type.
    /// - [`crate::Error::Crypto`] when the participant_key
    ///   derivation fails (practically unreachable with
    ///   valid leaf indices).
    pub fn try_open_with_overlap(
        &self,
        kid: u64,
        aad: &[u8],
        ciphertext: &[u8],
        frame_counter: u64,
    ) -> Result<Vec<u8>> {
        self.ring
            .try_open_with_overlap(kid, aad, ciphertext, frame_counter)
    }

    /// Drop the previous-epoch seed. Caller invokes this
    /// when its 1-second overlap timer fires; after the
    /// call, frames sealed under the OLD seed cease to
    /// decode and seed-boundary forward secrecy is fully
    /// in effect.
    ///
    /// Idempotent — calling without a stashed previous
    /// seed is a no-op.
    pub fn drop_previous_call_key(&mut self) {
        self.ring.drop_previous();
    }

    /// Drop the previous-epoch key once it has expired at `now_ms` (G-18). The
    /// expiry was set by [`Self::refresh_call_key_at`]; the platform calls this
    /// on its clock tick instead of owning the 1-second decision. Idempotent —
    /// a no-op when there is no previous key, or it is still inside its window,
    /// or it was retained with no expiry (via [`Self::refresh_call_key`]).
    pub fn prune_expired_previous(&mut self, now_ms: u64) {
        self.ring.prune_expired(now_ms);
    }

    /// `true` when a previous-epoch key is retained AND still inside its
    /// overlap window at `now_ms`. A key retained with no expiry (via
    /// [`Self::refresh_call_key`]) is always live until dropped.
    #[must_use]
    pub fn has_live_previous(&self, now_ms: u64) -> bool {
        self.ring.has_live_previous(now_ms)
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
    use crate::call_key_ring::{OVERLAP_DEFAULT_MS, OVERLAP_MAX_MS, OVERLAP_MIN_MS};
    use crate::header::SframeHeader;
    use lakko_mls::Identity as MlsIdentity;

    /// The composite KID a receiver reads from the header for a sender leaf's
    /// first (stream-0) SFrame stream (ADR-0035). These tests exercise one
    /// stream per leaf; the sender assigns stream 0 to its own `own_participant_key(0)`.
    fn kid(sender_leaf: u32) -> u64 {
        SframeHeader::kid_for(sender_leaf, 0).expect("stream 0 fits")
    }

    fn make_pair() -> (Identity, Identity, Group) {
        let id_a = MlsIdentity::generate("device-A").unwrap();
        let id_b = MlsIdentity::generate("device-B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let mut group_a = lakko_mls::Group::create(&id_a, lakko_mls::GroupKind::Group).unwrap();
        let addition = group_a.add_member(&id_a, kp_b).unwrap();
        let _welcome_bytes = lakko_mls::group::encode_message(&addition.welcome).unwrap();
        (id_a, id_b, group_a)
    }

    /// Same shape as [`make_pair`] but also returns B's
    /// independently-derived `Group` handle (built from the
    /// Welcome). The cross-party seal/open tests need both
    /// sides' Groups so the exporter is exercised twice on
    /// independent state — the way it'll be in production.
    fn make_pair_joined() -> (Identity, Identity, Group, Group) {
        let id_a = MlsIdentity::generate("device-A").unwrap();
        let id_b = MlsIdentity::generate("device-B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let mut group_a = lakko_mls::Group::create(&id_a, lakko_mls::GroupKind::Group).unwrap();
        let addition = group_a.add_member(&id_a, kp_b).unwrap();
        let welcome_bytes = lakko_mls::group::encode_message(&addition.welcome).unwrap();
        let group_b = lakko_mls::Group::join_from_welcome(&welcome_bytes, &id_b).unwrap();
        (id_a, id_b, group_a, group_b)
    }

    fn fixture_call_id() -> CallId {
        let mut id = [0u8; 16];
        id[0] = 0x10;
        id[15] = 0x10;
        id
    }

    fn fixture_session() -> (GroupCallSession, Group, Identity) {
        let (id_a, _id_b, group_a) = make_pair();
        let session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();
        (session, group_a, id_a)
    }

    /// Advance the keying group's MLS epoch by adding a throwaway member, so the
    /// next `refresh_call_key*` derives a DISTINCT seed — a genuine rotation.
    /// The S2 same-seed-refresh no-op (crypto audit) means the overlap-window
    /// *timing* tests must drive a real rotation, not a same-epoch refresh (a
    /// same-seed refresh is now correctly a no-op that stashes no previous key).
    fn advance_epoch(group: &mut Group, adder: &Identity, device_tag: &str) {
        let newcomer = MlsIdentity::generate(device_tag).unwrap();
        let kp = newcomer.create_key_package().unwrap();
        let _ = group.add_member(adder, kp).unwrap();
    }

    // ---- G-18: clock-injected overlap-window expiry --------------------------

    #[test]
    fn refresh_at_retains_previous_until_expiry_then_prune_drops_it() {
        let (mut s, mut g, id) = fixture_session();
        assert!(s.previous_call_key().is_none(), "no previous at creation");

        // Advance the epoch so the refresh derives a DISTINCT seed — a genuine
        // rotation (a same-seed refresh is now a no-op, S2).
        advance_epoch(&mut g, &id, "device-C");
        // Rotate at t=1000 with a 1 s window → previous expires at 2000.
        s.refresh_call_key_at(&g, &id, 1_000, OVERLAP_DEFAULT_MS)
            .unwrap();
        assert!(s.previous_call_key().is_some());
        assert!(s.has_live_previous(1_999), "live one ms before expiry");
        assert!(!s.has_live_previous(2_000), "expired at the boundary");

        // Prune before expiry is a no-op; at/after expiry drops it.
        s.prune_expired_previous(1_500);
        assert!(
            s.previous_call_key().is_some(),
            "pruning mid-window keeps it"
        );
        s.prune_expired_previous(2_000);
        assert!(
            s.previous_call_key().is_none(),
            "pruning at expiry drops it"
        );
    }

    #[test]
    fn refresh_at_clamps_the_overlap_window() {
        // Below the floor clamps up to OVERLAP_MIN_MS.
        let (mut s, mut g, id) = fixture_session();
        advance_epoch(&mut g, &id, "device-C");
        s.refresh_call_key_at(&g, &id, 1_000, 1).unwrap();
        assert!(s.has_live_previous(1_000 + OVERLAP_MIN_MS - 1));
        assert!(!s.has_live_previous(1_000 + OVERLAP_MIN_MS));

        // Above the ceiling clamps down to OVERLAP_MAX_MS.
        let (mut s2, mut g2, id2) = fixture_session();
        advance_epoch(&mut g2, &id2, "device-C2");
        s2.refresh_call_key_at(&g2, &id2, 0, u64::MAX).unwrap();
        assert!(s2.has_live_previous(OVERLAP_MAX_MS - 1));
        assert!(!s2.has_live_previous(OVERLAP_MAX_MS));
    }

    #[test]
    fn refresh_without_clock_never_auto_expires() {
        let (mut s, mut g, id) = fixture_session();
        // Advance the epoch so the refresh is a genuine rotation (distinct seed).
        advance_epoch(&mut g, &id, "device-C");
        // The legacy no-clock path retains the previous key with no expiry.
        s.refresh_call_key(&g, &id).unwrap();
        // The no-clock path parks the expiry at the u64::MAX sentinel, which no
        // realistic millisecond clock (~584M years) ever reaches.
        assert!(
            s.has_live_previous(u64::MAX - 1),
            "live at any realistic clock"
        );
        s.prune_expired_previous(u64::MAX - 1); // never reaches the sentinel → no-op
        assert!(s.previous_call_key().is_some());
        // Only the explicit manual drop removes it.
        s.drop_previous_call_key();
        assert!(s.previous_call_key().is_none());
    }

    #[test]
    fn second_rotation_resets_the_window_to_the_latest() {
        let (mut s, mut g, id) = fixture_session();
        advance_epoch(&mut g, &id, "device-C");
        s.refresh_call_key_at(&g, &id, 1_000, OVERLAP_DEFAULT_MS)
            .unwrap();
        // A second rotation at t=5000 (another epoch advance → another distinct
        // seed) replaces the retained previous (C-10: only the most-recent
        // previous epoch is kept) and resets the window.
        advance_epoch(&mut g, &id, "device-D");
        s.refresh_call_key_at(&g, &id, 5_000, OVERLAP_DEFAULT_MS)
            .unwrap();
        assert!(
            s.has_live_previous(5_999),
            "window tracks the latest rotation"
        );
        assert!(!s.has_live_previous(6_000));
        // The first window (expiring at 2000) is gone — no stale long-lived key.
        s.prune_expired_previous(2_500);
        assert!(
            s.previous_call_key().is_some(),
            "the latest window is still live"
        );
    }

    #[test]
    fn refresh_at_saturates_near_clock_max() {
        let (mut s, mut g, id) = fixture_session();
        advance_epoch(&mut g, &id, "device-C");
        // now_ms + overlap must not overflow.
        s.refresh_call_key_at(&g, &id, u64::MAX, OVERLAP_DEFAULT_MS)
            .unwrap();
        assert!(s.previous_call_key().is_some());
        // Expiry saturated to u64::MAX → not "live" exactly at MAX, but no panic.
        assert!(!s.has_live_previous(u64::MAX));
    }

    /// Defence in depth (ADR-0069 keeps the guard): a `refresh_call_key*` that
    /// re-derives the SAME seed (no MLS epoch advanced) is a total no-op — it
    /// stashes NO previous key, leaves the current seed unchanged, and does NOT
    /// raise `needs_rekey`, since nothing rotated. A genuine rotation (a
    /// distinct seed) is unaffected (proven by the timing tests above).
    #[test]
    fn same_seed_refresh_is_a_noop_and_does_not_flag_rekey() {
        let (mut s, g, id) = fixture_session();
        let seed_before = *s.call_key().as_bytes();

        // Refresh against the SAME group at the SAME epoch → identical seed.
        s.refresh_call_key(&g, &id).unwrap();
        assert!(!s.needs_rekey(), "same-seed refresh must not flag a rekey");
        assert!(
            s.previous_call_key().is_none(),
            "same-seed refresh must stash no previous key"
        );
        assert_eq!(
            *s.call_key().as_bytes(),
            seed_before,
            "current seed unchanged by a same-seed refresh"
        );

        // The clock-aware variant is equally a no-op — no window opens.
        s.refresh_call_key_at(&g, &id, 1_000, OVERLAP_DEFAULT_MS)
            .unwrap();
        assert!(!s.needs_rekey());
        assert!(s.previous_call_key().is_none());
        assert!(!s.has_live_previous(1_000));
        assert_eq!(*s.call_key().as_bytes(), seed_before);
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
        let k_self = session.own_participant_key(0).unwrap();
        let k_remote = session.remote_participant_key(kid(1)).unwrap();
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
        let mut group_a = lakko_mls::Group::create(&id_a, lakko_mls::GroupKind::Group).unwrap();
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

    // =====================================================
    // Phase 3c4 — cross-party SFrame seal/open via two
    // independent GroupCallSession instances. Exercises the
    // real MLS exporter agreement across an A↔B pair (and an
    // A↔B↔C trio) rather than the shortcut used in
    // `sframe::tests::participant_key_round_trips`.
    // =====================================================

    /// A seals with its own_participant_key; B derives A's
    /// remote_participant_key from B's independent MLS Group
    /// view and opens. This is the real production shape —
    /// no shared seed, no shortcut — proving the MLS
    /// exporter agrees byte-for-byte across the wire.
    #[test]
    fn cross_party_seal_open_round_trips() {
        let (id_a, id_b, group_a, group_b) = make_pair_joined();
        let call_id = fixture_call_id();

        let session_a =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_a, &id_a).unwrap();
        let session_b =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_b, &id_b).unwrap();

        // Both sides agree on the call_key seed.
        assert_eq!(
            session_a.call_key().as_bytes(),
            session_b.call_key().as_bytes()
        );
        // And on the local-leaf indices each thinks they own.
        assert_eq!(session_a.own_leaf_index, 0);
        assert_eq!(session_b.own_leaf_index, 1);

        // A → B: A seals with its own key (sender_leaf = 0);
        // B re-derives A's key from B's side via
        // remote_participant_key(0) and opens.
        let mut send_a = session_a.own_participant_key(0).unwrap();
        let recv_a_on_b = session_b.remote_participant_key(kid(0)).unwrap();
        let aad = b"sframe-header-A";
        let pt = b"hello from A";
        let (ctr, ct) = send_a.seal(aad, pt).unwrap();
        let opened = recv_a_on_b.open(aad, &ct, ctr).unwrap();
        assert_eq!(opened.as_slice(), pt.as_ref());

        // B → A: symmetric path. Confirms the per-leaf
        // derivation is not biased toward leaf 0.
        let mut send_b = session_b.own_participant_key(0).unwrap();
        let recv_b_on_a = session_a.remote_participant_key(kid(1)).unwrap();
        let aad = b"sframe-header-B";
        let pt = b"hello from B";
        let (ctr, ct) = send_b.seal(aad, pt).unwrap();
        let opened = recv_b_on_a.open(aad, &ct, ctr).unwrap();
        assert_eq!(opened.as_slice(), pt.as_ref());
    }

    /// After an MLS commit that advances the chat group's
    /// epoch (here: A adds a third member C), both A and B
    /// `refresh_call_key`. The post-epoch call_key seed
    /// differs from the pre-epoch one. Cross-party seal/open
    /// still works between A↔B under the new seed; frames
    /// sealed under the OLD key cannot be opened with the
    /// NEW key — the rotation gives forward secrecy of the
    /// seed boundary, even before the 1s SFrame overlap
    /// window applies.
    #[test]
    fn cross_party_round_trip_survives_epoch_advance() {
        let id_a = MlsIdentity::generate("device-A").unwrap();
        let id_b = MlsIdentity::generate("device-B").unwrap();
        let id_c = MlsIdentity::generate("device-C").unwrap();
        let call_id = fixture_call_id();

        // Epoch 0 — A creates, B joins.
        let mut group_a = lakko_mls::Group::create(&id_a, lakko_mls::GroupKind::Group).unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let addition_b = group_a.add_member(&id_a, kp_b).unwrap();
        let welcome_b = lakko_mls::group::encode_message(&addition_b.welcome).unwrap();
        let mut group_b = lakko_mls::Group::join_from_welcome(&welcome_b, &id_b).unwrap();

        let mut session_a =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_a, &id_a).unwrap();
        let mut session_b =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_b, &id_b).unwrap();
        let seed_pre = *session_a.call_key().as_bytes();
        assert_eq!(seed_pre, *session_b.call_key().as_bytes());

        // Snapshot a stale ciphertext under the OLD key, AAD,
        // and counter. We'll try to open it after rotation
        // and expect AEAD authentication failure.
        let mut send_a_pre = session_a.own_participant_key(0).unwrap();
        let stale_aad = b"sframe-header-pre";
        let stale_plain = b"stale frame";
        let (stale_counter, stale_cipher) = send_a_pre.seal(stale_aad, stale_plain).unwrap();

        // Epoch advance — A adds C; B applies the commit.
        let kp_c = id_c.create_key_package().unwrap();
        let addition_c = group_a.add_member(&id_a, kp_c).unwrap();
        let commit_bytes = lakko_mls::group::encode_message(&addition_c.commit).unwrap();
        let processed = group_b.process_incoming(&id_b, &commit_bytes).unwrap();
        assert!(matches!(processed, lakko_mls::Incoming::Commit));

        session_a.refresh_call_key(&group_a, &id_a).unwrap();
        session_b.refresh_call_key(&group_b, &id_b).unwrap();
        let seed_post = *session_a.call_key().as_bytes();
        assert_ne!(seed_pre, seed_post, "epoch advance must rotate seed");
        assert_eq!(seed_post, *session_b.call_key().as_bytes());
        assert!(session_a.needs_rekey());
        assert!(session_b.needs_rekey());
        session_a.ack_rekey();
        session_b.ack_rekey();

        // Cross-party seal/open under the NEW seed: works.
        let mut send_a_post = session_a.own_participant_key(0).unwrap();
        let recv_a_on_b = session_b.remote_participant_key(kid(0)).unwrap();
        let fresh_aad = b"sframe-header-post";
        let fresh_plain = b"fresh frame";
        let (fresh_counter, fresh_cipher) = send_a_post.seal(fresh_aad, fresh_plain).unwrap();
        let opened = recv_a_on_b
            .open(fresh_aad, &fresh_cipher, fresh_counter)
            .unwrap();
        assert_eq!(opened.as_slice(), fresh_plain.as_ref());

        // Stale ciphertext under the OLD key cannot be opened
        // by the NEW key — AEAD authentication rejects.
        // This is the "seed boundary forward secrecy" the
        // re-key buys before the 1s overlap window even
        // applies (the overlap holds the old key briefly on
        // the wire; here we're past that).
        let recv_a_stale = session_b.remote_participant_key(kid(0)).unwrap();
        match recv_a_stale.open(stale_aad, &stale_cipher, stale_counter) {
            Ok(_) => panic!("stale frame must not decode under new key"),
            Err(crate::Error::Crypto(_)) => {},
            Err(other) => panic!("expected Crypto error, got {other:?}"),
        }
    }

    /// Three-party seal/open: every participant can seal
    /// under its own_participant_key and every other
    /// participant can open via the sender's
    /// remote_participant_key. Exercises the
    /// "everyone derives everyone else's key" invariant the
    /// SFU forwarding path depends on.
    #[test]
    fn three_party_full_mesh_seal_open() {
        let id_a = MlsIdentity::generate("device-A").unwrap();
        let id_b = MlsIdentity::generate("device-B").unwrap();
        let id_c = MlsIdentity::generate("device-C").unwrap();
        let call_id = fixture_call_id();

        // Build the 3-party group: A creates, A adds B
        // (commit-1), A adds C (commit-2). B and C catch up
        // by applying the relevant commits / welcome.
        let mut group_a = lakko_mls::Group::create(&id_a, lakko_mls::GroupKind::Group).unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let add_b = group_a.add_member(&id_a, kp_b).unwrap();
        let welcome_b = lakko_mls::group::encode_message(&add_b.welcome).unwrap();
        let mut group_b = lakko_mls::Group::join_from_welcome(&welcome_b, &id_b).unwrap();

        let kp_c = id_c.create_key_package().unwrap();
        let add_c = group_a.add_member(&id_a, kp_c).unwrap();
        let welcome_c = lakko_mls::group::encode_message(&add_c.welcome).unwrap();
        let group_c = lakko_mls::Group::join_from_welcome(&welcome_c, &id_c).unwrap();
        // B applies the commit that added C so B's epoch
        // catches up to A's and C's.
        let commit_c_bytes = lakko_mls::group::encode_message(&add_c.commit).unwrap();
        let processed = group_b.process_incoming(&id_b, &commit_c_bytes).unwrap();
        assert!(matches!(processed, lakko_mls::Incoming::Commit));

        let session_a =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_a, &id_a).unwrap();
        let session_b =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_b, &id_b).unwrap();
        let session_c =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_c, &id_c).unwrap();

        // Everyone agrees on the seed.
        let seed = *session_a.call_key().as_bytes();
        assert_eq!(seed, *session_b.call_key().as_bytes());
        assert_eq!(seed, *session_c.call_key().as_bytes());
        assert_eq!(session_a.own_leaf_index, 0);
        assert_eq!(session_b.own_leaf_index, 1);
        assert_eq!(session_c.own_leaf_index, 2);

        // Full mesh: each of A / B / C seals one frame; the
        // other two open it via the sender's leaf index.
        let senders: [(u32, &GroupCallSession); 3] =
            [(0, &session_a), (1, &session_b), (2, &session_c)];
        let receivers: [&GroupCallSession; 3] = [&session_a, &session_b, &session_c];

        for (sender_leaf, sender_session) in senders {
            let mut send_key = sender_session.own_participant_key(0).unwrap();
            let aad = format!("sframe/leaf={sender_leaf}");
            let pt = format!("frame-from-{sender_leaf}");
            let (ctr, ct) = send_key.seal(aad.as_bytes(), pt.as_bytes()).unwrap();

            for (idx, receiver) in receivers.iter().enumerate() {
                if u32::try_from(idx).unwrap() == sender_leaf {
                    // Skip the sender itself; the SFU loops
                    // a participant's own frames back, but
                    // SFrame open under one's own key is a
                    // separate property (counter would clash
                    // — receiver state holds the latest
                    // observed counter per sender).
                    continue;
                }
                let recv_key = receiver.remote_participant_key(kid(sender_leaf)).unwrap();
                let opened = recv_key.open(aad.as_bytes(), &ct, ctr).unwrap();
                assert_eq!(opened.as_slice(), pt.as_bytes());
            }
        }
    }

    // =====================================================
    // Phase 3c5 — Mid-call Add/Remove + 1s overlap window.
    // The crate stays wall-clock-free; the caller (platform
    // SFrame-transform layer) invokes drop_previous_call_key
    // after its own 1s timer fires.
    // =====================================================

    /// During the overlap window (after `refresh_call_key`
    /// but before `drop_previous_call_key`), a frame sealed
    /// under the OLD seed still decodes via
    /// `try_open_with_overlap`. A fresh frame sealed under
    /// the NEW seed also decodes. Once `drop_previous_call_key`
    /// fires, the OLD-seed frame ceases to decode but the
    /// NEW-seed one still does.
    #[test]
    fn overlap_window_accepts_stale_frame_until_dropped() {
        let id_a = MlsIdentity::generate("device-A").unwrap();
        let id_b = MlsIdentity::generate("device-B").unwrap();
        let id_c = MlsIdentity::generate("device-C").unwrap();
        let call_id = fixture_call_id();

        // Epoch 0 — A creates, B joins.
        let mut group_a = lakko_mls::Group::create(&id_a, lakko_mls::GroupKind::Group).unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let add_b = group_a.add_member(&id_a, kp_b).unwrap();
        let welcome_b = lakko_mls::group::encode_message(&add_b.welcome).unwrap();
        let mut group_b = lakko_mls::Group::join_from_welcome(&welcome_b, &id_b).unwrap();

        let mut session_a =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_a, &id_a).unwrap();
        let mut session_b =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_b, &id_b).unwrap();

        // Pre-rotation frame from A → B (sealed under epoch-0 key).
        let mut send_a_pre = session_a.own_participant_key(0).unwrap();
        let stale_aad = b"sframe-header-pre";
        let stale_plain = b"in-flight frame";
        let (stale_counter, stale_cipher) = send_a_pre.seal(stale_aad, stale_plain).unwrap();

        // Epoch advance — A adds C, B applies the commit.
        let kp_c = id_c.create_key_package().unwrap();
        let add_c = group_a.add_member(&id_a, kp_c).unwrap();
        let commit = lakko_mls::group::encode_message(&add_c.commit).unwrap();
        let processed = group_b.process_incoming(&id_b, &commit).unwrap();
        assert!(matches!(processed, lakko_mls::Incoming::Commit));
        session_a.refresh_call_key(&group_a, &id_a).unwrap();
        session_b.refresh_call_key(&group_b, &id_b).unwrap();
        assert!(session_b.previous_call_key().is_some(), "overlap stashed");

        // Overlap path on the B side: stale frame still
        // decodes via the previous-key fallback.
        let opened = session_b
            .try_open_with_overlap(kid(0), stale_aad, &stale_cipher, stale_counter)
            .unwrap();
        assert_eq!(opened.as_slice(), stale_plain.as_ref());

        // Fresh frame under the NEW seed also decodes via
        // the current-key path.
        let mut send_a_post = session_a.own_participant_key(0).unwrap();
        let fresh_aad = b"sframe-header-post";
        let fresh_plain = b"post-rotation frame";
        let (fresh_counter, fresh_cipher) = send_a_post.seal(fresh_aad, fresh_plain).unwrap();
        let opened = session_b
            .try_open_with_overlap(kid(0), fresh_aad, &fresh_cipher, fresh_counter)
            .unwrap();
        assert_eq!(opened.as_slice(), fresh_plain.as_ref());

        // Platform's overlap timer fires.
        session_b.drop_previous_call_key();
        assert!(session_b.previous_call_key().is_none());

        // Stale frame now rejects — seed-boundary forward
        // secrecy fully in effect.
        match session_b.try_open_with_overlap(kid(0), stale_aad, &stale_cipher, stale_counter) {
            Ok(_) => panic!("stale frame must not decode after overlap drop"),
            Err(crate::Error::Crypto(_)) => {},
            Err(other) => panic!("expected Crypto error, got {other:?}"),
        }

        // Fresh frame still decodes — current path
        // unaffected. We re-seal under a fresh counter (the
        // prior seal mutated the sender's counter).
        let (fresh_counter2, fresh_cipher2) = send_a_post.seal(fresh_aad, fresh_plain).unwrap();
        let opened = session_b
            .try_open_with_overlap(kid(0), fresh_aad, &fresh_cipher2, fresh_counter2)
            .unwrap();
        assert_eq!(opened.as_slice(), fresh_plain.as_ref());
    }

    /// Receiver outside an overlap window (no rotation has
    /// happened yet) — `try_open_with_overlap` collapses to
    /// the current-key-only path. Fresh frames decode;
    /// random-bit garbage rejects.
    #[test]
    fn no_overlap_window_collapses_to_current_only() {
        let (id_a, id_b, group_a, group_b) = make_pair_joined();
        let call_id = fixture_call_id();
        let session_a =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_a, &id_a).unwrap();
        let session_b =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_b, &id_b).unwrap();
        assert!(
            session_b.previous_call_key().is_none(),
            "no overlap initially"
        );

        let mut send_a = session_a.own_participant_key(0).unwrap();
        let aad = b"sframe-header";
        let plain = b"frame";
        let (counter, cipher) = send_a.seal(aad, plain).unwrap();
        let opened = session_b
            .try_open_with_overlap(kid(0), aad, &cipher, counter)
            .unwrap();
        assert_eq!(opened.as_slice(), plain.as_ref());

        // Tamper one byte → rejects, and the error message
        // explicitly says no overlap window is active (to
        // help platform-side debugging differentiate "we
        // missed a rotation" from "the wire is corrupted").
        let mut bad_cipher = cipher.clone();
        bad_cipher[0] ^= 0xFF;
        match session_b.try_open_with_overlap(kid(0), aad, &bad_cipher, counter) {
            Ok(_) => panic!("tampered frame must not decode"),
            Err(crate::Error::Crypto(msg)) => {
                assert!(
                    msg.contains("no overlap window"),
                    "expected no-overlap-window hint, got: {msg}"
                );
            },
            Err(other) => panic!("expected Crypto error, got {other:?}"),
        }
    }

    /// Two rotations in rapid succession — only the
    /// most-recent previous seed is retained per the
    /// doc-comment contract. A frame sealed under
    /// epoch 0 decodes during the epoch-1 overlap. After
    /// rotating to epoch 2, frames sealed under epoch 1
    /// decode during the epoch-2 overlap, but epoch-0
    /// frames no longer decode (their seed got displaced).
    #[test]
    fn chained_rotations_retain_only_most_recent_previous() {
        let id_a = MlsIdentity::generate("device-A").unwrap();
        let id_b = MlsIdentity::generate("device-B").unwrap();
        let id_c = MlsIdentity::generate("device-C").unwrap();
        let id_d = MlsIdentity::generate("device-D").unwrap();
        let call_id = fixture_call_id();

        // Epoch 0: A creates, B joins.
        let mut group_a = lakko_mls::Group::create(&id_a, lakko_mls::GroupKind::Group).unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let add_b = group_a.add_member(&id_a, kp_b).unwrap();
        let welcome_b = lakko_mls::group::encode_message(&add_b.welcome).unwrap();
        let mut group_b = lakko_mls::Group::join_from_welcome(&welcome_b, &id_b).unwrap();

        let mut session_a =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_a, &id_a).unwrap();
        let mut session_b =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_b, &id_b).unwrap();

        // Frame F0 sealed under epoch-0 key.
        let mut send_a_e0 = session_a.own_participant_key(0).unwrap();
        let aad_e0 = b"epoch-0";
        let plain_e0 = b"frame from epoch 0";
        let (counter_e0, cipher_e0) = send_a_e0.seal(aad_e0, plain_e0).unwrap();

        // Epoch advance to 1 — A adds C; B applies.
        let kp_c = id_c.create_key_package().unwrap();
        let add_c = group_a.add_member(&id_a, kp_c).unwrap();
        let commit_c = lakko_mls::group::encode_message(&add_c.commit).unwrap();
        group_b.process_incoming(&id_b, &commit_c).unwrap();
        session_a.refresh_call_key(&group_a, &id_a).unwrap();
        session_b.refresh_call_key(&group_b, &id_b).unwrap();

        // F0 still decodes (epoch-1 overlap covers epoch-0).
        let opened = session_b
            .try_open_with_overlap(kid(0), aad_e0, &cipher_e0, counter_e0)
            .unwrap();
        assert_eq!(opened.as_slice(), plain_e0.as_ref());

        // Frame F1 sealed under epoch-1 key.
        let mut send_a_e1 = session_a.own_participant_key(0).unwrap();
        let aad_e1 = b"epoch-1";
        let plain_e1 = b"frame from epoch 1";
        let (counter_e1, cipher_e1) = send_a_e1.seal(aad_e1, plain_e1).unwrap();

        // Epoch advance to 2 — A adds D; B applies. The
        // epoch-1 seed displaces the epoch-0 seed in the
        // previous_call_key slot.
        let kp_d = id_d.create_key_package().unwrap();
        let add_d = group_a.add_member(&id_a, kp_d).unwrap();
        let commit_d = lakko_mls::group::encode_message(&add_d.commit).unwrap();
        group_b.process_incoming(&id_b, &commit_d).unwrap();
        session_a.refresh_call_key(&group_a, &id_a).unwrap();
        session_b.refresh_call_key(&group_b, &id_b).unwrap();

        // F1 still decodes (epoch-2 overlap covers epoch-1).
        let opened = session_b
            .try_open_with_overlap(kid(0), aad_e1, &cipher_e1, counter_e1)
            .unwrap();
        assert_eq!(opened.as_slice(), plain_e1.as_ref());

        // F0 no longer decodes — its seed got pushed out of
        // the single overlap slot. Per the doc-comment
        // contract, only the most-recent previous epoch is
        // retained.
        match session_b.try_open_with_overlap(kid(0), aad_e0, &cipher_e0, counter_e0) {
            Ok(_) => panic!("epoch-0 frame must not decode in epoch-2 overlap"),
            Err(crate::Error::Crypto(_)) => {},
            Err(other) => panic!("expected Crypto error, got {other:?}"),
        }
    }

    /// `drop_previous_call_key` is idempotent — calling
    /// without an active overlap is a no-op, not a panic.
    #[test]
    fn drop_previous_call_key_is_idempotent() {
        let (id_a, _id_b, group_a) = make_pair();
        let mut session =
            GroupCallSession::create(GroupCallScope::InChat, fixture_call_id(), &group_a, &id_a)
                .unwrap();
        assert!(session.previous_call_key().is_none());
        session.drop_previous_call_key();
        session.drop_previous_call_key();
        assert!(session.previous_call_key().is_none());
    }

    /// AAD-binding check on the cross-party path: A seals
    /// with `aad_a`; B tries to open with `aad_b`. The
    /// AEAD construction must reject. This is the
    /// participant-key-level twin of the SFrame
    /// header-integrity property (the SFrame header itself
    /// is the AAD in production).
    #[test]
    fn cross_party_wrong_aad_rejects() {
        let (id_a, id_b, group_a, group_b) = make_pair_joined();
        let call_id = fixture_call_id();

        let session_a =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_a, &id_a).unwrap();
        let session_b =
            GroupCallSession::create(GroupCallScope::InChat, call_id, &group_b, &id_b).unwrap();

        let mut send_a = session_a.own_participant_key(0).unwrap();
        let recv_a_on_b = session_b.remote_participant_key(kid(0)).unwrap();
        let (ctr, ct) = send_a.seal(b"aad-A", b"frame").unwrap();
        match recv_a_on_b.open(b"aad-B", &ct, ctr) {
            Ok(_) => panic!("wrong AAD must fail authentication"),
            Err(crate::Error::Crypto(_)) => {},
            Err(other) => panic!("expected Crypto error, got {other:?}"),
        }
    }
}
