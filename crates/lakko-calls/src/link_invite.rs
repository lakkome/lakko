// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Joiner-side orchestration for link-based group-call
//! invites (ROADMAP 3c3-joiner).
//!
//! The wire codec ships in [`lakko_contacts::call_invite`]
//! (3c3). This module turns a verified
//! [`lakko_contacts::CallInviteCapability`] into a live
//! MLS group + a [`crate::GroupCallSession`] ready to seal /
//! open SFrame frames — the actual joining step.
//!
//! ## Flow
//!
//! ```text
//!   owner Alice                          joiner Bob
//!      │                                    │
//!      │ — composes CallInviteCapability    │
//!      │   embedding her one-time KP        │
//!      │ — publishes the link               │
//!      │                                    │
//!      │      link out-of-band              │
//!      │ ────────────────────────────────►  │
//!      │                                    │
//!      │                                    │ — decode_and_verify (3c3 codec)
//!      │                                    │ — accept_call_invite (HERE)
//!      │                                    │
//!      │                                    │    creates a fresh MLS group with
//!      │                                    │    Bob as founder, adds Alice via
//!      │                                    │    the embedded KP, builds a
//!      │                                    │    GroupCallSession (scope =
//!      │                                    │    LinkBased) keyed by the
//!      │                                    │    capability's call_id.
//!      │                                    │
//!      │   Welcome (returned to Bob)        │
//!      │ ◄──────────────────────────────────│  delivery channel is up to the
//!      │                                    │  platform (typically the
//!      │                                    │  owner's stranger-inbox via
//!      │                                    │  capability.owner_account_id).
//!      │                                    │
//!      │ — Group::join_from_welcome         │
//!      │ — GroupCallSession::create with     │
//!      │   the same call_id                  │
//!      │                                    │
//!      │ both sides derive matching         │
//!      │ CallKey via the MLS exporter        │
//!      │ (3a4 / 3c4 invariants C-1, C-2)    │
//! ```
//!
//! ## Two topologies
//!
//! **1×1 fast-path (ADR-0009), [`accept_call_invite`].** The
//! joiner founds the group and adds the owner via the
//! capability's embedded one-time `KeyPackage`. Single-use by
//! construction — a second joiner reusing the same capability
//! would call `Group::create` on its own side and land in a
//! separate MLS group (same `CallKey` seed from the same
//! `call_id`, but a different epoch → the call would not
//! connect). Retained for the 1×1 case.
//!
//! **Multi-joiner shareable links (ADR-0032), owner-founds.**
//! For a link many strangers join, the **owner is the sole
//! founder** ([`found_call`]); joiners never `Group::create`,
//! so there is exactly one group and no founding race (the
//! G-21 two-groups bug is closed by design). Each joiner mints
//! its own fresh `KeyPackage` ([`request_call_join`]), the
//! acting committer adds it ([`admit_joiner`]), and the joiner
//! enters via the returned Welcome ([`join_call_from_welcome`]).
//! The committer of record is [`elect_committer`] — the owner
//! while present, else the lowest-leaf present member — so the
//! call survives the owner leaving mid-call (failover) and
//! reverts to the owner on return (reclaim), with no election
//! protocol. The capability's embedded one-time owner-KP is
//! unused on this path, so the one-time-KP-in-a-shared-link
//! problem disappears. Transport for the KeyPackage / Welcome
//! exchange is the SFU `signal-relay` (live call) or the
//! stranger-inbox [`lakko_contacts::wrap_call_kp_forward`] /
//! [`lakko_contacts::wrap_call_welcome`] ferry (cold start).
//!
//! ## What this module does NOT do
//!
//! - Decode the capability bytes — caller drives
//!   [`lakko_contacts::call_invite::decode_and_verify`]
//!   first and hands us the validated struct.
//! - Check `is_valid_now(now_ms)` — caller's job. We
//!   accept any capability; expiry is a UX concern.
//! - Deliver the Welcome to Alice. That's the platform's
//!   transport layer (stranger-inbox / DS journal /
//!   QR-mediated paste).
//! - Enforce `max_uses` / per-joiner dedup — the discovery
//!   `/c/<blob_id>` blob enforces `max_uses`, and the acting
//!   committer dedups joins by `(call_id, joiner leaf key)`
//!   (ADR-0032 G-22). This module just founds / admits / joins.

use lakko_contacts::CallInviteCapability;
use lakko_mls::group::encode_message;
use lakko_mls::identity::Identity;
use lakko_mls::{Group, GroupKind};

use crate::error::{Error, Result};
use crate::group_call::{GroupCallScope, GroupCallSession};

/// Output of [`accept_call_invite`]. Three things bind
/// together: the new MLS group (mutable, the joiner now
/// owns it), the [`GroupCallSession`] ledger keyed against
/// it, and the Welcome bytes the joiner needs to deliver
/// to the owner to complete the join.
pub struct AcceptedCallInvite {
    /// Fresh MLS group with the joiner as founder + the
    /// owner already added.
    pub group: Group,
    /// Pre-bootstrapped `GroupCallSession` for the
    /// link-based call. `set_joined(0, true)` has NOT been
    /// called — the platform decides when the joiner
    /// considers themselves "on the call" (typically
    /// after the SFU handshake or, in the SFU-less P2P
    /// case, after the Welcome lands on the owner's side).
    pub session: GroupCallSession,
    /// Welcome message bytes (MLS wire format) the joiner
    /// must deliver to the owner. The owner runs
    /// `Group::join_from_welcome` on it to enter the
    /// call group.
    pub welcome_for_owner: Vec<u8>,
}

/// Bootstrap the joiner side of a link-based group call.
///
/// Builds a fresh MLS group with `joiner_identity` as
/// founder, adds the owner via the capability's embedded
/// `one_time_key_package`, derives a
/// [`GroupCallSession`] keyed by `capability.call_id`,
/// and returns the Welcome bytes for the joiner to
/// deliver to the owner.
///
/// ## Caller responsibilities
///
/// - Run [`lakko_contacts::call_invite::decode_and_verify`]
///   on the capability bytes before calling this — we
///   trust the [`CallInviteCapability`] struct.
/// - Optionally check `capability.is_valid_now(now_ms)`;
///   we don't.
/// - Deliver `welcome_for_owner` to the owner via some
///   transport (stranger-inbox routed by
///   `capability.owner_account_id`, a QR-mediated paste,
///   etc.).
///
/// ## Errors
///
/// - [`Error::Crypto`] when the embedded KP is malformed
///   (wrong ciphersuite, signature invalid, etc. — the
///   MLS layer surfaces a single typed shape here).
/// - [`Error::Exporter`] when the MLS exporter rejects
///   the call-key derivation (practically unreachable
///   with valid inputs).
pub fn accept_call_invite(
    capability: &CallInviteCapability,
    joiner_identity: &Identity,
) -> Result<AcceptedCallInvite> {
    // 1. Joiner founds a fresh MLS group. Per ROADMAP 3c3
    //    "call MLS group spins up on first accepted
    //    joiner" — this is that step.
    // ADR-0062: a call-link group is a Group — ADR-0032 admits joiners
    //    into it, so its membership grows.
    let mut group = Group::create(joiner_identity, GroupKind::Group)
        .map_err(|e| Error::Crypto(format!("accept_call_invite Group::create: {e}")))?;

    // 2. Add the owner via the embedded one-time
    //    KeyPackage. The MLS layer validates the KP +
    //    produces a Welcome we'll ferry to the owner.
    let addition = group
        .add_member_from_wire(joiner_identity, &capability.one_time_key_package)
        .map_err(|e| Error::Crypto(format!("accept_call_invite add_member_from_wire: {e}")))?;

    let welcome_for_owner = encode_message(&addition.welcome)
        .map_err(|e| Error::Crypto(format!("accept_call_invite encode Welcome: {e}")))?;

    // 3. Build the GroupCallSession ledger keyed against
    //    the new group + the capability's call_id. Scope =
    //    LinkBased so subsequent observers can distinguish
    //    link calls from in-chat calls.
    let session = GroupCallSession::create(
        GroupCallScope::LinkBased,
        capability.call_id,
        &group,
        joiner_identity,
    )?;

    Ok(AcceptedCallInvite {
        group,
        session,
        welcome_for_owner,
    })
}

/// A candidate committer in a link-based call, as a client observes it by
/// joining the MLS membership (leaf index) with the SFU roster (presence) and
/// the capability (`owner_account_id`).
///
/// The mapping from an MLS leaf to an account / presence is app-level (the SFU
/// per-call peer tag + the signed capability), so this type carries the already
/// resolved facts rather than reaching into MLS internals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitterCandidate {
    /// The member's MLS leaf index in the call group.
    pub leaf_index: u32,
    /// `true` if this member is the call's owner — its account matches the
    /// capability's `owner_account_id`.
    pub is_owner: bool,
    /// `true` if this member is currently present (connected to the SFU room).
    pub is_present: bool,
}

/// Deterministically elect the committer-of-record for a link-based call
/// ([ADR-0032](../../../docs/adr/0032-multi-joiner-call-links.md)).
///
/// The committer is the **owner if the owner is present**, otherwise the
/// **present member with the lowest leaf index**. Returns the elected leaf
/// index, or `None` when no member is present (a transient state with no
/// committer — e.g. everyone momentarily disconnected).
///
/// The election is a **pure, total function of the `(roster, presence)`
/// snapshot**: every member computes the same answer from the same snapshot, so
/// failover (the owner drops → the lowest-leaf successor takes over) and reclaim
/// (the owner returns → the rule reverts to the owner) need no election protocol
/// and cannot split-brain. It reads only public state (leaf index, ownership,
/// presence) — no key material, no MLS mutation.
///
/// Among present owners (there should be exactly one; a mis-flagging caller is
/// tolerated) the lowest leaf wins, so the result is deterministic regardless of
/// candidate ordering.
#[must_use]
pub fn elect_committer(candidates: &[CommitterCandidate]) -> Option<u32> {
    // Owner-if-present wins outright. Take the lowest-leaf present owner so a
    // caller that mis-flags two owners still gets a deterministic answer.
    let owner_leaf = candidates
        .iter()
        .filter(|c| c.is_owner && c.is_present)
        .map(|c| c.leaf_index)
        .min();
    if owner_leaf.is_some() {
        return owner_leaf;
    }
    // No present owner → the lowest-leaf present member is the acting committer.
    candidates
        .iter()
        .filter(|c| c.is_present)
        .map(|c| c.leaf_index)
        .min()
}

// =============================================================================
// Owner-founds orchestration (ADR-0032, multi-joiner shareable links)
// =============================================================================
//
// Unlike `accept_call_invite` (the ADR-0009 1×1 fast-path, where the joiner
// founds the group and adds the owner via the capability's embedded one-time
// KeyPackage), the multi-joiner path makes the OWNER the sole founder. Joiners
// never call `Group::create`, so there is exactly one MLS group and no founding
// race (the G-21 two-groups bug). Each joiner mints its own fresh KeyPackage
// (`request_call_join`), the acting committer adds it (`admit_joiner`), and the
// joiner enters via the returned Welcome (`join_call_from_welcome`). The
// committer of record is `elect_committer` above, so the flow survives the owner
// leaving mid-call.

/// The owner's freshly-founded link call — the MLS group (owner at leaf 0) plus
/// its [`GroupCallSession`] ledger keyed by `call_id`.
pub struct FoundedCall {
    /// Fresh MLS group with the owner as the sole founder (leaf 0).
    pub group: Group,
    /// `GroupCallSession` for the link-based call.
    pub session: GroupCallSession,
}

/// Found the owner side of a multi-joiner link call ([ADR-0032]).
///
/// The owner creates a fresh MLS group (it is leaf 0, the sole founder) and the
/// `GroupCallSession` keyed by `call_id`. Joiners are added later via
/// [`admit_joiner`]; no joiner ever founds, so there is only ever one group.
///
/// # Errors
///
/// [`Error::Crypto`] if `Group::create` fails; [`Error::Exporter`] if the
/// call-key derivation is rejected.
///
/// [ADR-0032]: ../../../docs/adr/0032-multi-joiner-call-links.md
pub fn found_call(owner_identity: &Identity, call_id: [u8; 16]) -> Result<FoundedCall> {
    // ADR-0062: call-link groups grow (ADR-0032), so they are a Group.
    let group = Group::create(owner_identity, GroupKind::Group)
        .map_err(|e| Error::Crypto(format!("found_call Group::create: {e}")))?;
    let session =
        GroupCallSession::create(GroupCallScope::LinkBased, call_id, &group, owner_identity)?;
    Ok(FoundedCall { group, session })
}

/// Mint the joiner's request to join a multi-joiner link call ([ADR-0032]).
///
/// Produces a fresh serialized `KeyPackage` for `joiner_identity` (its private
/// half persists in the identity's own provider, so the same identity can later
/// open the Welcome in [`join_call_from_welcome`]). The caller ferries these
/// bytes to the acting committer — over the SFU `signal-relay` for a live call,
/// or the stranger-inbox `wrap_call_kp_forward` for a cold start. The joiner
/// never founds a group.
///
/// On a cold start the owner's mailbox is closed to strangers (ADR-0060): the
/// joiner writes the `ContactRequest` carrying the forward under the
/// capability's `link` grant, and puts its own `reply` grant inside that
/// request (`lakko_contacts::request::compose`) so the Welcome can come back
/// to its closed mailbox.
///
/// # Errors
///
/// [`Error::Crypto`] if the KeyPackage cannot be minted or serialized.
///
/// [ADR-0032]: ../../../docs/adr/0032-multi-joiner-call-links.md
pub fn request_call_join(joiner_identity: &Identity) -> Result<Vec<u8>> {
    let kp = joiner_identity
        .create_key_package()
        .map_err(|e| Error::Crypto(format!("request_call_join create_key_package: {e}")))?;
    lakko_mls::key_package::serialize(&kp)
        .map_err(|e| Error::Crypto(format!("request_call_join serialize KeyPackage: {e}")))
}

/// Result of [`admit_joiner`] — the two MLS messages a committer produces when
/// adding a joiner: the commit for the existing members, and the Welcome for the
/// new joiner.
pub struct AdmittedJoiner {
    /// Commit bytes (MLS wire format) to broadcast to the EXISTING members; each
    /// runs `Group::process_incoming` on it to advance to the new epoch.
    pub commit_for_members: Vec<u8>,
    /// Welcome bytes (MLS wire format) to deliver to the NEW joiner; it runs
    /// [`join_call_from_welcome`] on it to enter the call group.
    pub welcome_for_joiner: Vec<u8>,
}

/// The acting committer admits a joiner into a multi-joiner link call
/// ([ADR-0032]).
///
/// Adds `joiner_key_package` (the bytes from [`request_call_join`]) to `group`,
/// advancing `group` to the new epoch, and returns the commit (for the existing
/// members) + the Welcome (for the joiner). The caller MUST already be the
/// [`elect_committer`]-designated committer — a redundant commit from a
/// non-committer would fork the epoch; the deterministic election is what keeps
/// exactly one committer active.
///
/// # Errors
///
/// [`Error::Crypto`] if the KeyPackage is malformed or the MLS add fails.
///
/// [ADR-0032]: ../../../docs/adr/0032-multi-joiner-call-links.md
pub fn admit_joiner(
    group: &mut Group,
    committer_identity: &Identity,
    joiner_key_package: &[u8],
) -> Result<AdmittedJoiner> {
    let addition = group
        .add_member_from_wire(committer_identity, joiner_key_package)
        .map_err(|e| Error::Crypto(format!("admit_joiner add_member_from_wire: {e}")))?;
    let commit_for_members = encode_message(&addition.commit)
        .map_err(|e| Error::Crypto(format!("admit_joiner encode commit: {e}")))?;
    let welcome_for_joiner = encode_message(&addition.welcome)
        .map_err(|e| Error::Crypto(format!("admit_joiner encode Welcome: {e}")))?;
    Ok(AdmittedJoiner {
        commit_for_members,
        welcome_for_joiner,
    })
}

/// A joiner that has entered a multi-joiner link call via a Welcome — the MLS
/// group plus its [`GroupCallSession`] keyed by `call_id`.
pub struct JoinedCall {
    /// The joined MLS group (the joiner is now a member).
    pub group: Group,
    /// `GroupCallSession` for the link-based call, agreeing on the same
    /// `CallKey` seed as every other member at this epoch.
    pub session: GroupCallSession,
}

/// The joiner enters a multi-joiner link call from the committer's Welcome
/// ([ADR-0032]).
///
/// Runs `Group::join_from_welcome` (using the private half persisted by the
/// earlier [`request_call_join`] under the SAME `joiner_identity`) and builds
/// the `GroupCallSession` keyed by `call_id`. On success the joiner shares the
/// group view + derives the same `CallKey` seed as every other member.
///
/// # Errors
///
/// [`Error::Crypto`] if the Welcome is malformed or references a KeyPackage the
/// joiner's provider does not hold; [`Error::Exporter`] on call-key derivation.
///
/// [ADR-0032]: ../../../docs/adr/0032-multi-joiner-call-links.md
pub fn join_call_from_welcome(
    welcome_bytes: &[u8],
    joiner_identity: &Identity,
    call_id: [u8; 16],
) -> Result<JoinedCall> {
    let group = Group::join_from_welcome(welcome_bytes, joiner_identity)
        .map_err(|e| Error::Crypto(format!("join_call_from_welcome join_from_welcome: {e}")))?;
    let session =
        GroupCallSession::create(GroupCallScope::LinkBased, call_id, &group, joiner_identity)?;
    Ok(JoinedCall { group, session })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::SframeHeader;
    use lakko_contacts::call_invite::{compose, CallInviteMedia};
    use lakko_crypto::signing::SigningKey;
    use lakko_mls::Identity as MlsIdentity;

    fn signing_for(identity: &MlsIdentity) -> SigningKey {
        SigningKey::from_seed(&identity.ed25519_seed_bytes())
    }

    /// The composite KID a receiver reads from the header for a sender leaf's
    /// first (stream-0) SFrame stream (ADR-0035).
    fn kid(sender_leaf: u32) -> u64 {
        SframeHeader::kid_for(sender_leaf, 0).expect("stream 0 fits")
    }

    const TEST_LINK_ID: [u8; 16] = [0x1C; 16];

    fn test_link_grant() -> lakko_contacts::reach::ReachGrant {
        lakko_contacts::reach::ReachGrant::derive(
            &[0x6E; 32],
            lakko_contacts::reach::ReachClass::Link,
            &TEST_LINK_ID,
            0,
        )
        .unwrap()
    }

    fn fixture_account_id() -> [u8; 16] {
        let mut id = [0_u8; 16];
        id[0] = 0x01;
        id[15] = 0xAB;
        id
    }

    fn fixture_call_id() -> [u8; 16] {
        let mut id = [0_u8; 16];
        id[0] = 0x10;
        id[15] = 0x10;
        id
    }

    // -------------------------------------------------------------------------
    // elect_committer (ADR-0032 deterministic committer election)
    // -------------------------------------------------------------------------

    fn cand(leaf: u32, is_owner: bool, is_present: bool) -> CommitterCandidate {
        CommitterCandidate {
            leaf_index: leaf,
            is_owner,
            is_present,
        }
    }

    #[test]
    fn elect_owner_when_owner_present() {
        // Owner is leaf 3 (not the lowest) but present → owner wins.
        let c = [
            cand(0, false, true),
            cand(3, true, true),
            cand(5, false, true),
        ];
        assert_eq!(elect_committer(&c), Some(3));
    }

    #[test]
    fn elect_lowest_leaf_when_owner_absent() {
        // Owner (leaf 3) absent → lowest present leaf (1) is committer.
        let c = [
            cand(1, false, true),
            cand(3, true, false),
            cand(5, false, true),
        ];
        assert_eq!(elect_committer(&c), Some(1));
    }

    #[test]
    fn elect_reclaim_reverts_to_owner_on_return() {
        // Owner absent → successor leaf 1; owner returns → back to owner (leaf 2).
        let absent = [cand(1, false, true), cand(2, true, false)];
        assert_eq!(elect_committer(&absent), Some(1));
        let present = [cand(1, false, true), cand(2, true, true)];
        assert_eq!(elect_committer(&present), Some(2));
    }

    #[test]
    fn elect_none_when_no_one_present() {
        let c = [cand(0, true, false), cand(1, false, false)];
        assert_eq!(elect_committer(&c), None);
    }

    #[test]
    fn elect_ignores_absent_lower_leaf() {
        // Leaf 0 is absent; the lowest PRESENT leaf (2) wins, not 0.
        let c = [
            cand(0, false, false),
            cand(2, false, true),
            cand(4, false, true),
        ];
        assert_eq!(elect_committer(&c), Some(2));
    }

    #[test]
    fn elect_deterministic_with_mis_flagged_double_owner() {
        // Defensive: two present owners → lowest leaf, order-independent.
        let a = [
            cand(5, true, true),
            cand(2, true, true),
            cand(9, false, true),
        ];
        let b = [
            cand(9, false, true),
            cand(2, true, true),
            cand(5, true, true),
        ];
        assert_eq!(elect_committer(&a), Some(2));
        assert_eq!(elect_committer(&b), Some(2));
    }

    #[test]
    fn elect_empty_is_none() {
        assert_eq!(elect_committer(&[]), None);
    }

    // -------------------------------------------------------------------------
    // Owner-founds multi-joiner orchestration E2E (ADR-0032)
    // -------------------------------------------------------------------------

    /// Full owner-founds flow with failover + reclaim. Alice (owner) founds;
    /// Bob + Carol join via `request_call_join` / `admit_joiner` /
    /// `join_call_from_welcome`; every active member agrees on the epoch +
    /// `CallKey` seed. Alice then drops → `elect_committer` picks Bob (the
    /// lowest present leaf) → Bob admits Dave as the acting committer. Alice
    /// returns, catches up on the missed commit, and the election reclaims the
    /// committer role to her (owner present). Closes the ADR-0032 core loop.
    #[test]
    #[allow(clippy::too_many_lines)] // E2E walkthrough by design.
    fn owner_founds_multi_joiner_with_failover_and_reclaim() {
        let alice = MlsIdentity::generate("alice").unwrap();
        let bob = MlsIdentity::generate("bob").unwrap();
        let carol = MlsIdentity::generate("carol").unwrap();
        let dave = MlsIdentity::generate("dave").unwrap();
        let call_id = fixture_call_id();

        // Fresh CallKey seed for a member's CURRENT group state — the session
        // must be rebuilt after each commit to read the seed at the new epoch.
        let seed = |g: &Group, id: &MlsIdentity| {
            GroupCallSession::create(GroupCallScope::LinkBased, call_id, g, id)
                .unwrap()
                .call_key()
                .as_bytes()
                .to_vec()
        };

        // --- Alice founds; she is the sole founder at leaf 0.
        let mut alice_call = found_call(&alice, call_id).unwrap();
        assert_eq!(alice_call.group.own_leaf_index().u32(), 0);

        // --- Bob requests → Alice (committer) admits → Bob joins. Only Alice
        //     was a member, so her group already advanced via the add; there is
        //     no other existing member to process the commit.
        let bob_kp = request_call_join(&bob).unwrap();
        let admit_bob = admit_joiner(&mut alice_call.group, &alice, &bob_kp).unwrap();
        let mut bob_call =
            join_call_from_welcome(&admit_bob.welcome_for_joiner, &bob, call_id).unwrap();
        assert_eq!(alice_call.group.epoch(), bob_call.group.epoch());
        assert_eq!(seed(&alice_call.group, &alice), seed(&bob_call.group, &bob));

        // --- Carol requests → Alice admits → Carol joins; Bob (existing
        //     member) processes the commit to advance to the same epoch.
        let carol_kp = request_call_join(&carol).unwrap();
        let admit_carol = admit_joiner(&mut alice_call.group, &alice, &carol_kp).unwrap();
        bob_call
            .group
            .process_incoming(&bob, &admit_carol.commit_for_members)
            .unwrap();
        let mut carol_call =
            join_call_from_welcome(&admit_carol.welcome_for_joiner, &carol, call_id).unwrap();
        assert_eq!(alice_call.group.epoch(), bob_call.group.epoch());
        assert_eq!(alice_call.group.epoch(), carol_call.group.epoch());
        assert_eq!(
            seed(&alice_call.group, &alice),
            seed(&carol_call.group, &carol)
        );

        // --- FAILOVER: Alice (owner, leaf 0) drops. The remaining members'
        //     roster → election picks Bob (leaf 1, the lowest PRESENT leaf).
        let roster_owner_absent = [
            cand(0, true, false), // Alice: owner, absent
            cand(1, false, true), // Bob:   present
            cand(2, false, true), // Carol: present
        ];
        assert_eq!(elect_committer(&roster_owner_absent), Some(1));

        // --- Bob (acting committer) admits Dave; Carol (existing member)
        //     processes the commit. Alice is offline — her local view stays
        //     behind at the post-Carol epoch until she returns and catches up.
        let dave_kp = request_call_join(&dave).unwrap();
        let admit_dave = admit_joiner(&mut bob_call.group, &bob, &dave_kp).unwrap();
        carol_call
            .group
            .process_incoming(&carol, &admit_dave.commit_for_members)
            .unwrap();
        let dave_call =
            join_call_from_welcome(&admit_dave.welcome_for_joiner, &dave, call_id).unwrap();
        assert_eq!(bob_call.group.epoch(), carol_call.group.epoch());
        assert_eq!(bob_call.group.epoch(), dave_call.group.epoch());
        assert_eq!(seed(&bob_call.group, &bob), seed(&dave_call.group, &dave));

        // --- RECLAIM: Alice returns. She admitted Bob + Carol herself, so she
        //     missed only Dave's commit; processing it catches her up to the
        //     post-Dave epoch, and the election reverts the committer to her.
        alice_call
            .group
            .process_incoming(&alice, &admit_dave.commit_for_members)
            .unwrap();
        assert_eq!(alice_call.group.epoch(), dave_call.group.epoch());
        assert_eq!(
            seed(&alice_call.group, &alice),
            seed(&dave_call.group, &dave)
        );
        let roster_owner_back = [
            cand(0, true, true),  // Alice: owner, back
            cand(1, false, true), // Bob
            cand(2, false, true), // Carol
            cand(3, false, true), // Dave
        ];
        assert_eq!(elect_committer(&roster_owner_back), Some(0));
    }

    /// Owner Alice mints a capability; joiner Bob accepts;
    /// both end up in the same MLS group at the same
    /// epoch, deriving the same `CallKey` seed.
    #[test]
    fn accept_invite_round_trips_via_welcome() {
        let alice = MlsIdentity::generate("alice").unwrap();
        let bob = MlsIdentity::generate("bob").unwrap();

        // Alice mints her one-time KP for the call invite
        // + the wire-encoded capability bytes.
        let alice_kp = alice.create_key_package().unwrap();
        let alice_kp_bytes = lakko_mls::key_package::serialize(&alice_kp).unwrap();
        let alice_signing = signing_for(&alice);
        let cap_wire = compose(
            &alice_signing,
            &fixture_account_id(),
            &fixture_call_id(),
            0, // never expires
            0, // single use
            CallInviteMedia::audio_video(),
            &alice_kp_bytes,
            "Alice's standup",
            &TEST_LINK_ID,
            &test_link_grant(),
        )
        .unwrap();
        let capability = lakko_contacts::call_invite::decode_and_verify(&cap_wire).unwrap();

        // Bob accepts the invite — builds his MLS group +
        // session + Welcome.
        let accepted = accept_call_invite(&capability, &bob).unwrap();

        // Alice's side: she runs join_from_welcome on the
        // bytes Bob shipped over.
        let mut alice_group =
            Group::join_from_welcome(&accepted.welcome_for_owner, &alice).unwrap();

        // Both sides build a GroupCallSession against the
        // same call_id; the call_key seeds MUST match.
        let alice_session = GroupCallSession::create(
            GroupCallScope::LinkBased,
            fixture_call_id(),
            &alice_group,
            &alice,
        )
        .unwrap();

        assert_eq!(
            accepted.session.call_key().as_bytes(),
            alice_session.call_key().as_bytes(),
            "call_key seeds must agree across the invite-accept boundary"
        );

        // Alice and Bob can also seal / open frames to
        // each other (full 3c4 invariant C-2 holds across
        // this bootstrap path).
        let mut send_bob = accepted.session.own_participant_key(0).unwrap();
        let recv_bob_on_alice = alice_session
            .remote_participant_key(kid(accepted.session.own_leaf_index))
            .unwrap();
        let aad = b"sframe-header";
        let pt = b"hello from Bob over a link-based call";
        let (ctr, ct) = send_bob.seal(aad, pt).unwrap();
        let opened = recv_bob_on_alice.open(aad, &ct, ctr).unwrap();
        assert_eq!(opened.as_slice(), pt.as_ref());

        // Sanity: epoch agrees too (Bob added Alice so
        // both should be at epoch 1).
        assert_eq!(accepted.group.epoch(), alice_group.epoch());
        let _ = &mut alice_group; // suppress unused-mut warning
    }

    /// Capability with a corrupted KP must reject at
    /// `accept_call_invite` time — the MLS layer's
    /// validate step refuses the malformed bytes.
    #[test]
    fn accept_invite_with_corrupted_kp_rejects() {
        let alice = MlsIdentity::generate("alice").unwrap();
        let bob = MlsIdentity::generate("bob").unwrap();

        let alice_kp = alice.create_key_package().unwrap();
        let mut alice_kp_bytes = lakko_mls::key_package::serialize(&alice_kp).unwrap();
        // Flip a byte deep in the KP — far enough in to
        // mangle a TLS-codec field rather than the leading
        // protocol-version constant (which has the same
        // value on every key package, so a flip would be
        // trivially noticed at boundary parsing).
        alice_kp_bytes[120] ^= 0xFF;
        let alice_signing = signing_for(&alice);
        // The capability codec re-signs the wire so the
        // outer signature remains valid; only the KP
        // payload is corrupted.
        let cap_wire = compose(
            &alice_signing,
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &alice_kp_bytes,
            "",
            &TEST_LINK_ID,
            &test_link_grant(),
        )
        .unwrap();
        let capability = lakko_contacts::call_invite::decode_and_verify(&cap_wire).unwrap();

        match accept_call_invite(&capability, &bob) {
            Err(Error::Crypto(msg)) => {
                assert!(
                    msg.contains("add_member_from_wire") || msg.contains("KeyPackage"),
                    "expected MLS-layer failure, got: {msg}"
                );
            },
            Err(other) => panic!("expected Crypto error, got {other:?}"),
            Ok(_) => panic!("expected error, got success"),
        }
    }

    /// `LinkBased` scope is preserved end-to-end —
    /// platform UX may surface this differently from
    /// `InChat`.
    #[test]
    fn accept_invite_uses_link_based_scope() {
        let alice = MlsIdentity::generate("alice").unwrap();
        let bob = MlsIdentity::generate("bob").unwrap();
        let alice_kp = alice.create_key_package().unwrap();
        let alice_kp_bytes = lakko_mls::key_package::serialize(&alice_kp).unwrap();
        let cap_wire = compose(
            &signing_for(&alice),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &alice_kp_bytes,
            "",
            &TEST_LINK_ID,
            &test_link_grant(),
        )
        .unwrap();
        let capability = lakko_contacts::call_invite::decode_and_verify(&cap_wire).unwrap();
        let accepted = accept_call_invite(&capability, &bob).unwrap();
        assert_eq!(accepted.session.scope, GroupCallScope::LinkBased);
    }

    /// Joiner's `own_leaf_index` is 0 (they're the founder
    /// of the call group). Owner ends up at leaf 1 after
    /// joining via Welcome. This matches the ROADMAP 3c3
    /// "first accepted joiner founds the group" contract.
    #[test]
    fn joiner_is_founder_at_leaf_0() {
        let alice = MlsIdentity::generate("alice").unwrap();
        let bob = MlsIdentity::generate("bob").unwrap();
        let alice_kp = alice.create_key_package().unwrap();
        let alice_kp_bytes = lakko_mls::key_package::serialize(&alice_kp).unwrap();
        let cap_wire = compose(
            &signing_for(&alice),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &alice_kp_bytes,
            "",
            &TEST_LINK_ID,
            &test_link_grant(),
        )
        .unwrap();
        let capability = lakko_contacts::call_invite::decode_and_verify(&cap_wire).unwrap();
        let accepted = accept_call_invite(&capability, &bob).unwrap();
        assert_eq!(accepted.session.own_leaf_index, 0);

        // Alice joins via Welcome — she lands at leaf 1.
        let alice_group = Group::join_from_welcome(&accepted.welcome_for_owner, &alice).unwrap();
        assert_eq!(alice_group.own_leaf_index().u32(), 1);
    }

    // ====================================================================
    // E2E coverage — full link-call wire chain
    // (Phase 3c3-link-call-e2e-test)
    // ====================================================================

    /// 1×1 link-call E2E. Walks the **full** wire chain a
    /// production deployment would drive — composer →
    /// acceptor → welcome-ferry wrap → incoming-envelope
    /// dispatcher → MLS join. Any future refactor that
    /// breaks the chain at any point surfaces here as a
    /// single named failure rather than as a silent UX
    /// regression in the platform polling loops.
    ///
    /// Steps mapped to the audit-pack invariants they
    /// exercise (full chain per docs/security/calls.md):
    ///
    ///   1. Owner composes a `CallInviteCapability` —
    ///      C-24..C-26.
    ///   2. Joiner runs `accept_call_invite` — C-27 (call-
    ///      key seed agreement) is set up here.
    ///   3. Joiner wraps `welcomeForOwner` via
    ///      `wrap_call_welcome` — C-32.
    ///   4. Owner side: `dispatch_incoming_envelope` runs on
    ///      the wrapped plaintext — C-34. Asserts the
    ///      `CallWelcome` variant is returned (NOT
    ///      `IntroMessage`, NOT `CallKeyPackage`).
    ///   5. Owner runs `Group::join_from_welcome` on the
    ///      unwrapped payload.
    ///   6. Both sides build `GroupCallSession`s against
    ///      the same `call_id` — call-key seeds MUST agree
    ///      byte-for-byte (C-27).
    ///   7. Joiner and owner exchange a sealed SFrame frame
    ///      in both directions — full media-path round-trip.
    #[test]
    fn link_call_full_e2e_via_welcome_envelope() {
        use lakko_contacts::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};
        use lakko_contacts::wrap_call_welcome;

        let alice = MlsIdentity::generate("alice").unwrap();
        let bob = MlsIdentity::generate("bob").unwrap();

        // (1) Owner composes the capability.
        let alice_kp = alice.create_key_package().unwrap();
        let alice_kp_bytes = lakko_mls::key_package::serialize(&alice_kp).unwrap();
        let cap_wire = compose(
            &signing_for(&alice),
            &fixture_account_id(),
            &fixture_call_id(),
            0, // never expires
            0, // single use
            CallInviteMedia::audio_video(),
            &alice_kp_bytes,
            "Alice's standup",
            &TEST_LINK_ID,
            &test_link_grant(),
        )
        .unwrap();
        let capability = lakko_contacts::call_invite::decode_and_verify(&cap_wire).unwrap();

        // (2) Joiner runs accept_call_invite.
        let accepted = accept_call_invite(&capability, &bob).unwrap();

        // (3) Joiner wraps the Welcome bytes inside the
        //     call-welcome envelope domain separator.
        let wrapped_welcome = wrap_call_welcome(&accepted.welcome_for_owner);

        // (4) Owner's polling loop runs the dispatcher on
        //     the wrapped plaintext (in production this
        //     plaintext came out of `open_envelope` on a
        //     stranger-inbox ContactRequest; here we feed
        //     the wrapped bytes directly since this test
        //     scopes the contract to the envelope codec,
        //     not the outer HPKE seal).
        let dispatched =
            dispatch_incoming_envelope(&wrapped_welcome).expect("dispatch should succeed");
        let welcome_payload = match dispatched {
            IncomingEnvelope::CallWelcome(p) => p,
            other => panic!(
                "expected CallWelcome variant, got {:?}",
                core::mem::discriminant(&other),
            ),
        };
        // Round-trip sanity: the dispatched bytes equal the
        // original Welcome.
        assert_eq!(welcome_payload, accepted.welcome_for_owner.as_slice());

        // (5) Owner runs Group::join_from_welcome.
        let alice_group = Group::join_from_welcome(welcome_payload, &alice).unwrap();

        // (6) Both sides build sessions; call-key seeds
        //     must agree.
        let alice_session = GroupCallSession::create(
            GroupCallScope::LinkBased,
            fixture_call_id(),
            &alice_group,
            &alice,
        )
        .unwrap();
        assert_eq!(
            accepted.session.call_key().as_bytes(),
            alice_session.call_key().as_bytes(),
            "call_key seeds must agree across the full envelope chain (C-27 + C-32 + C-34)",
        );

        // (7) Bidirectional SFrame round-trip.
        let aad = b"sframe-header";
        let mut bob_send = accepted.session.own_participant_key(0).unwrap();
        let alice_recv_bob = alice_session
            .remote_participant_key(kid(accepted.session.own_leaf_index))
            .unwrap();
        let bob_to_alice = b"hello from Bob through the full wire chain";
        let (ctr, ct) = bob_send.seal(aad, bob_to_alice).unwrap();
        let opened = alice_recv_bob.open(aad, &ct, ctr).unwrap();
        assert_eq!(opened.as_slice(), bob_to_alice.as_ref());

        let mut alice_send = alice_session.own_participant_key(0).unwrap();
        let bob_recv_alice = accepted
            .session
            .remote_participant_key(kid(alice_session.own_leaf_index))
            .unwrap();
        let alice_to_bob = b"hello from Alice over the same call key";
        let (alice_counter, alice_cipher) = alice_send.seal(aad, alice_to_bob).unwrap();
        let opened_for_bob = bob_recv_alice
            .open(aad, &alice_cipher, alice_counter)
            .unwrap();
        assert_eq!(opened_for_bob.as_slice(), alice_to_bob.as_ref());
    }

    /// Negative sanity. Feeding a plain free-text plaintext
    /// (i.e. someone sending a normal ContactRequest, not a
    /// Welcome) MUST surface as `IntroMessage` from the
    /// dispatcher, not as a CallWelcome / CallKeyPackage
    /// false-positive. Catches a future regression where a
    /// drift in the domain-separator constants accidentally
    /// makes the prefix match a UTF-8 intro message.
    #[test]
    fn dispatcher_treats_plain_intro_message_as_intro_message() {
        use lakko_contacts::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};

        let intro: &[u8] = b"hi alice, it's bob - let's chat";
        let result = dispatch_incoming_envelope(intro).expect("dispatch should not error");
        assert_eq!(result, IncomingEnvelope::IntroMessage(intro));
    }

    /// E2E negative — the dispatcher MUST refuse a wrapped
    /// envelope when the trailing welcome payload is below
    /// the welcome-floor (32 bytes). Pins the truncation-
    /// rejection path of the chain: a malformed envelope
    /// surfaces as `Err`, NOT as a silent intro-message
    /// fallthrough that would later confuse the UI.
    #[test]
    fn dispatcher_rejects_truncated_welcome_in_full_chain() {
        use lakko_contacts::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1;
        use lakko_contacts::incoming_envelope::dispatch_incoming_envelope;
        use lakko_contacts::{CallWelcomeError, DispatchError};

        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(CALL_WELCOME_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[0u8; 8]); // 8 < 32
        let err = dispatch_incoming_envelope(&wrapped).unwrap_err();
        assert_eq!(
            err,
            DispatchError::Call(CallWelcomeError::TruncatedPayload { min: 32, got: 8 }),
            "truncated welcome must surface as Err, not as a silent intro-message fallthrough",
        );
    }

    /// Multi-party E2E (3 participants). Walks the full
    /// **subsequent-joiner** wire chain a production
    /// deployment would drive for a 3rd participant joining
    /// an already-established 2-party link call:
    ///
    ///   1. Alice (owner) composes capability + Bob (first
    ///      joiner) accepts — sets up A↔B group.
    ///   2. Alice joins via Welcome — now Bob (leaf 0) +
    ///      Alice (leaf 1) are in the same MLS group.
    ///   3. Charlie (subsequent joiner) mints a fresh KP +
    ///      wraps via `wrap_call_kp_forward`.
    ///   4. Bob (committer) runs `dispatch_incoming_envelope`
    ///      on the wrapped plaintext — MUST route to
    ///      `CallKeyPackage` variant (NOT `CallWelcome`,
    ///      NOT `IntroMessage`).
    ///   5. Bob runs `Group::add_member_from_wire(kp)` on
    ///      the existing call group → produces (commit,
    ///      welcome).
    ///   6. Bob wraps the resulting Welcome via
    ///      `wrap_call_welcome` to ferry it back to
    ///      Charlie.
    ///   7. Charlie unwraps via `dispatch_incoming_envelope`
    ///      → `CallWelcome` variant → runs
    ///      `Group::join_from_welcome` → lands at leaf 2.
    ///   8. Alice processes the commit Bob produced (step 5
    ///      side-output) → her group state advances to the
    ///      same new epoch.
    ///   9. All three (A, B, C) derive byte-identical
    ///      CallKey seeds against the same `call_id` —
    ///      C-27 holds across the multi-party path.
    #[test]
    #[allow(clippy::too_many_lines)] // E2E walkthrough by design — splitting would obscure the chain
    fn link_call_multi_party_e2e_via_kp_forward_envelope() {
        use lakko_contacts::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};
        use lakko_contacts::{wrap_call_kp_forward, wrap_call_welcome};
        use lakko_mls::group::encode_message;
        use lakko_mls::Group;

        // ---- Phase 1: A↔B link-call bootstrap (reuse the
        //                1×1 E2E flow). ---------------------
        let alice = MlsIdentity::generate("alice").unwrap();
        let bob = MlsIdentity::generate("bob").unwrap();
        let charlie = MlsIdentity::generate("charlie").unwrap();

        let alice_kp = alice.create_key_package().unwrap();
        let alice_kp_bytes = lakko_mls::key_package::serialize(&alice_kp).unwrap();
        let cap_wire = compose(
            &signing_for(&alice),
            &fixture_account_id(),
            &fixture_call_id(),
            0, // never expires
            // Important: NOT single-use — the capability needs to
            // accept multiple joiners. `u32::MAX` is the unlimited
            // sentinel.
            u32::MAX,
            CallInviteMedia::audio_video(),
            &alice_kp_bytes,
            "Alice's standup",
            &TEST_LINK_ID,
            &test_link_grant(),
        )
        .unwrap();
        let capability = lakko_contacts::call_invite::decode_and_verify(&cap_wire).unwrap();

        let bob_accepted = accept_call_invite(&capability, &bob).unwrap();
        let mut alice_group =
            Group::join_from_welcome(&bob_accepted.welcome_for_owner, &alice).unwrap();
        let mut bob_group = bob_accepted.group;

        // Sanity check the 2-party state before Charlie
        // joins.
        assert_eq!(bob_group.epoch(), alice_group.epoch());
        let pre_epoch = bob_group.epoch();

        // ---- Phase 2: Charlie's KP-forward send. ---------
        let charlie_kp = charlie.create_key_package().unwrap();
        let charlie_kp_bytes = lakko_mls::key_package::serialize(&charlie_kp).unwrap();
        let wrapped_kp = wrap_call_kp_forward(&charlie_kp_bytes);

        // ---- Phase 3: Bob (committer) routes the wrapped
        //                KP through the dispatcher. -------
        let dispatched = dispatch_incoming_envelope(&wrapped_kp).expect("dispatch should succeed");
        let charlie_kp_payload = match dispatched {
            IncomingEnvelope::CallKeyPackage(p) => p,
            other => panic!(
                "expected CallKeyPackage variant, got {:?}",
                core::mem::discriminant(&other),
            ),
        };

        // ---- Phase 4: Bob adds Charlie + advances the
        //                group. -------------------------
        let addition = bob_group
            .add_member_from_wire(&bob, charlie_kp_payload)
            .expect("add Charlie via wrapped KP");

        // Bob's local view has advanced; pre_epoch + 1.
        assert_eq!(bob_group.epoch(), pre_epoch + 1);

        // ---- Phase 5: Bob ferries the Welcome back to
        //                Charlie via wrap_call_welcome. ---
        let welcome_bytes = encode_message(&addition.welcome).unwrap();
        let wrapped_welcome_for_charlie = wrap_call_welcome(&welcome_bytes);

        // Charlie's polling loop routes the wrapped welcome.
        let dispatched_for_charlie = dispatch_incoming_envelope(&wrapped_welcome_for_charlie)
            .expect("dispatch should succeed");
        let welcome_payload = match dispatched_for_charlie {
            IncomingEnvelope::CallWelcome(p) => p,
            other => panic!(
                "expected CallWelcome variant for Charlie's join, got {:?}",
                core::mem::discriminant(&other),
            ),
        };
        let charlie_group = Group::join_from_welcome(welcome_payload, &charlie).unwrap();

        // ---- Phase 6: Alice processes Bob's commit and
        //                advances her group state. ------
        let commit_bytes = encode_message(&addition.commit).unwrap();
        let _ = alice_group
            .process_incoming(&alice, &commit_bytes)
            .expect("Alice processes Bob's commit");

        // ---- Phase 7: All three at the same new epoch. -
        assert_eq!(
            bob_group.epoch(),
            alice_group.epoch(),
            "Alice's view must advance after processing Bob's commit",
        );
        assert_eq!(
            alice_group.epoch(),
            charlie_group.epoch(),
            "Charlie's view (post-join) must match Alice + Bob's new epoch",
        );

        // ---- Phase 8: All three derive byte-identical
        //                CallKey seeds. -----------------
        let bob_session_after_add = GroupCallSession::create(
            GroupCallScope::LinkBased,
            fixture_call_id(),
            &bob_group,
            &bob,
        )
        .unwrap();
        let alice_session_after_commit = GroupCallSession::create(
            GroupCallScope::LinkBased,
            fixture_call_id(),
            &alice_group,
            &alice,
        )
        .unwrap();
        let charlie_session = GroupCallSession::create(
            GroupCallScope::LinkBased,
            fixture_call_id(),
            &charlie_group,
            &charlie,
        )
        .unwrap();

        // The strongest property: all three sessions agree
        // on the same SFrame seed for the same call_id at
        // the same epoch (multi-party C-27 + C-33 + C-34).
        assert_eq!(
            bob_session_after_add.call_key().as_bytes(),
            alice_session_after_commit.call_key().as_bytes(),
            "Bob + Alice CallKey seeds must agree after the add commit",
        );
        assert_eq!(
            alice_session_after_commit.call_key().as_bytes(),
            charlie_session.call_key().as_bytes(),
            "Charlie's CallKey seed must agree with Alice + Bob's post-add",
        );
    }

    /// Owner-handover E2E. After the multi-party rendezvous
    /// of `link_call_multi_party_e2e_via_kp_forward_envelope`,
    /// **Bob (the first joiner / current committer) leaves
    /// the call**. The capability's `owner_account_id`
    /// designates Alice; she picks up the committer role for
    /// the next joiner. A fourth party Dave joins via KP-
    /// forward and Alice (not Bob) adds him. Final state: A,
    /// C, D in one MLS group at the post-handover epoch; Bob
    /// gone; all three derive byte-identical CallKey seeds.
    ///
    /// Closes the "what happens when committer leaves" gap
    /// the multi-party flow left open (mentioned in
    /// `docs/security/known-gaps.md::G-21`).
    ///
    /// **Designated-committer rule** (this test pins the
    /// policy): the capability's `owner_account_id` is the
    /// **primary committer** for the call group across its
    /// lifetime. The owner is a member by construction
    /// (added via her embedded KeyPackage when the first
    /// joiner accepted), so a future committer-rotation does
    /// not require a separate election protocol — the policy
    /// is "owner if present, otherwise the lowest active
    /// leaf index". The test exercises the owner-present
    /// branch; the leaf-index fallback is documented for
    /// the no-owner-anymore edge case the V1 wire does not
    /// reach (owner can't leave: she's the capability
    /// holder).
    #[test]
    #[allow(clippy::too_many_lines)] // E2E walkthrough by design
    fn link_call_owner_handover_after_committer_leaves() {
        use lakko_contacts::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};
        use lakko_contacts::{wrap_call_kp_forward, wrap_call_welcome};
        use lakko_mls::group::encode_message;
        use lakko_mls::Group;

        // ---- Phase 1: Reuse the multi-party setup (A↔B↔C). -
        let alice = MlsIdentity::generate("alice").unwrap();
        let bob = MlsIdentity::generate("bob").unwrap();
        let charlie = MlsIdentity::generate("charlie").unwrap();
        let dave = MlsIdentity::generate("dave").unwrap();

        let alice_kp = alice.create_key_package().unwrap();
        let alice_kp_bytes = lakko_mls::key_package::serialize(&alice_kp).unwrap();
        let cap_wire = compose(
            &signing_for(&alice),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            u32::MAX, // unlimited
            CallInviteMedia::audio_video(),
            &alice_kp_bytes,
            "Alice's all-hands",
            &TEST_LINK_ID,
            &test_link_grant(),
        )
        .unwrap();
        let capability = lakko_contacts::call_invite::decode_and_verify(&cap_wire).unwrap();

        let bob_accepted = accept_call_invite(&capability, &bob).unwrap();
        let mut alice_group =
            Group::join_from_welcome(&bob_accepted.welcome_for_owner, &alice).unwrap();
        let mut bob_group = bob_accepted.group;

        // Charlie joins via KP-forward through Bob.
        let charlie_kp = charlie.create_key_package().unwrap();
        let charlie_kp_bytes = lakko_mls::key_package::serialize(&charlie_kp).unwrap();
        let wrapped_charlie_kp = wrap_call_kp_forward(&charlie_kp_bytes);
        let dispatched = dispatch_incoming_envelope(&wrapped_charlie_kp).unwrap();
        let charlie_kp_payload = match dispatched {
            IncomingEnvelope::CallKeyPackage(p) => p,
            other => panic!(
                "expected CallKeyPackage, got {:?}",
                core::mem::discriminant(&other)
            ),
        };
        let bob_addition = bob_group
            .add_member_from_wire(&bob, charlie_kp_payload)
            .unwrap();
        let bob_commit_bytes = encode_message(&bob_addition.commit).unwrap();
        let charlie_welcome_bytes = encode_message(&bob_addition.welcome).unwrap();
        let wrapped_welcome_for_charlie = wrap_call_welcome(&charlie_welcome_bytes);
        let dispatched_for_charlie =
            dispatch_incoming_envelope(&wrapped_welcome_for_charlie).unwrap();
        let charlie_welcome_payload = match dispatched_for_charlie {
            IncomingEnvelope::CallWelcome(p) => p,
            other => panic!(
                "expected CallWelcome, got {:?}",
                core::mem::discriminant(&other)
            ),
        };
        let mut charlie_group =
            Group::join_from_welcome(charlie_welcome_payload, &charlie).unwrap();
        alice_group
            .process_incoming(&alice, &bob_commit_bytes)
            .unwrap();

        // Snapshot the 3-party state.
        let epoch_before_handover = alice_group.epoch();
        assert_eq!(bob_group.epoch(), epoch_before_handover);
        assert_eq!(charlie_group.epoch(), epoch_before_handover);

        // ---- Phase 2: Bob disconnects (e.g. closes the
        //                app); Alice (owner) drives the
        //                removal commit as the designated
        //                handover policy. OpenMLS 0.8.x
        //                does not allow self-removal via
        //                `remove_members` (RFC 9420 +
        //                `CreateCommitError::CannotRemoveSelf`);
        //                self-removal would need
        //                `leave_group_via_self_remove`, but
        //                a disconnected client can't drive
        //                that anyway — someone else has to
        //                commit. The capability's
        //                `owner_account_id` designates
        //                Alice as the owner-committer, so
        //                she's the natural one to land the
        //                kick. ---------------------------
        let bob_leaf_in_alice_view = bob_group.own_leaf_index();
        let alice_remove_bob = alice_group
            .remove_member(&alice, bob_leaf_in_alice_view)
            .expect("Alice as owner removes Bob's stale leaf");
        let alice_remove_bytes = encode_message(&alice_remove_bob).unwrap();

        // Charlie processes Alice's remove commit.
        charlie_group
            .process_incoming(&charlie, &alice_remove_bytes)
            .unwrap();

        let epoch_after_leave = alice_group.epoch();
        assert!(
            epoch_after_leave > epoch_before_handover,
            "remove commit must advance the epoch",
        );
        assert_eq!(
            charlie_group.epoch(),
            epoch_after_leave,
            "Charlie's view must match Alice's post-remove",
        );

        // ---- Phase 3: Dave joins. The capability's
        //                owner_account_id designates Alice
        //                as the committer; she handles the
        //                KP-forward, not Bob (who is gone). -
        let dave_kp = dave.create_key_package().unwrap();
        let dave_kp_bytes = lakko_mls::key_package::serialize(&dave_kp).unwrap();
        let wrapped_dave_kp = wrap_call_kp_forward(&dave_kp_bytes);

        // Alice's polling loop sees the KP-forward (in
        // production this comes via her stranger-inbox
        // ContactRequest envelope path; here we feed the
        // unwrapped plaintext directly to scope this test
        // to the handover policy, not the HPKE seal).
        let dispatched_for_alice = dispatch_incoming_envelope(&wrapped_dave_kp).unwrap();
        let dave_kp_payload = match dispatched_for_alice {
            IncomingEnvelope::CallKeyPackage(p) => p,
            other => panic!(
                "expected CallKeyPackage for Dave's join, got {:?}",
                core::mem::discriminant(&other),
            ),
        };

        // Alice — NOT Bob — runs add_member_from_wire. This
        // is the load-bearing property the handover test
        // pins: the committer role transferred to the
        // owner without an explicit handshake.
        let alice_addition = alice_group
            .add_member_from_wire(&alice, dave_kp_payload)
            .expect("Alice as new committer adds Dave");
        let alice_commit_bytes = encode_message(&alice_addition.commit).unwrap();
        let dave_welcome_bytes = encode_message(&alice_addition.welcome).unwrap();

        // Dave joins via the welcome Alice ferried back.
        let wrapped_welcome_for_dave = wrap_call_welcome(&dave_welcome_bytes);
        let dispatched_for_dave = dispatch_incoming_envelope(&wrapped_welcome_for_dave).unwrap();
        let dave_welcome_payload = match dispatched_for_dave {
            IncomingEnvelope::CallWelcome(p) => p,
            other => panic!(
                "expected CallWelcome for Dave, got {:?}",
                core::mem::discriminant(&other),
            ),
        };
        let dave_group = Group::join_from_welcome(dave_welcome_payload, &dave).unwrap();

        // Charlie processes Alice's commit (broadcast side-
        // output) so his group view advances.
        charlie_group
            .process_incoming(&charlie, &alice_commit_bytes)
            .unwrap();

        // ---- Phase 4: All three live members (A, C, D) at
        //                the same post-handover epoch. ----
        let epoch_after_handover = alice_group.epoch();
        assert!(
            epoch_after_handover > epoch_after_leave,
            "Alice's add-Dave commit must advance the epoch past Bob's leave",
        );
        assert_eq!(
            charlie_group.epoch(),
            epoch_after_handover,
            "Charlie must advance after Alice's commit",
        );
        assert_eq!(
            dave_group.epoch(),
            epoch_after_handover,
            "Dave's view (post-join) must match Alice + Charlie",
        );

        // ---- Phase 5: All three derive byte-identical
        //                CallKey seeds. The handover
        //                preserved the call_id binding even
        //                though the committer changed. ---
        let alice_session = GroupCallSession::create(
            GroupCallScope::LinkBased,
            fixture_call_id(),
            &alice_group,
            &alice,
        )
        .unwrap();
        let charlie_session = GroupCallSession::create(
            GroupCallScope::LinkBased,
            fixture_call_id(),
            &charlie_group,
            &charlie,
        )
        .unwrap();
        let dave_session = GroupCallSession::create(
            GroupCallScope::LinkBased,
            fixture_call_id(),
            &dave_group,
            &dave,
        )
        .unwrap();

        assert_eq!(
            alice_session.call_key().as_bytes(),
            charlie_session.call_key().as_bytes(),
            "Alice + Charlie CallKey seeds must agree post-handover",
        );
        assert_eq!(
            charlie_session.call_key().as_bytes(),
            dave_session.call_key().as_bytes(),
            "Dave's CallKey seed must agree with Alice + Charlie post-handover",
        );

        // Bob's old seed from before he left MUST NOT match
        // the new post-handover seed (forward secrecy across
        // the leave + add commits — every epoch advances the
        // exporter output).
        let bob_session_pre_leave = GroupCallSession::create(
            GroupCallScope::LinkBased,
            fixture_call_id(),
            &bob_group,
            &bob,
        )
        .unwrap();
        assert_ne!(
            bob_session_pre_leave.call_key().as_bytes(),
            alice_session.call_key().as_bytes(),
            "Bob's pre-leave seed must differ from the post-handover seed (forward secrecy)",
        );
    }
}
