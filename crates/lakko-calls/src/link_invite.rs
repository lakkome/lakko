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
//! ## V1 scope: single joiner
//!
//! `CallInviteCapability` embeds **one** one-time
//! `KeyPackage`. The first call to `accept_call_invite`
//! consumes it; a second joiner trying to use the same
//! capability would create a separate MLS group (because
//! both would call `Group::create` as the founder). The
//! ledger would still derive the same `CallKey` seed from
//! the same `call_id` exporter context, but the joiners
//! wouldn't share a group view — the call wouldn't
//! actually connect.
//!
//! Multi-joiner support requires a server-side rendezvous
//! (`/call-invite/{blob_id}` discovery endpoint + KP
//! refresh) and is deferred to a 3c3-multi-use sprint.
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
//! - Track `max_uses` — V1 treats every capability as
//!   single-use at the orchestration layer. The wire-side
//!   `max_uses` hint is informational.

use lakko_contacts::CallInviteCapability;
use lakko_mls::group::encode_message;
use lakko_mls::identity::Identity;
use lakko_mls::Group;

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
    let mut group = Group::create(joiner_identity)
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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::call_invite::{compose, CallInviteMedia};
    use lakko_crypto::signing::SigningKey;
    use lakko_mls::Identity as MlsIdentity;

    fn signing_for(identity: &MlsIdentity) -> SigningKey {
        SigningKey::from_seed(&identity.ed25519_seed_bytes())
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
        let mut send_bob = accepted.session.own_participant_key().unwrap();
        let recv_bob_on_alice = alice_session
            .remote_participant_key(accepted.session.own_leaf_index)
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
        )
        .unwrap();
        let capability = lakko_contacts::call_invite::decode_and_verify(&cap_wire).unwrap();
        let accepted = accept_call_invite(&capability, &bob).unwrap();
        assert_eq!(accepted.session.own_leaf_index, 0);

        // Alice joins via Welcome — she lands at leaf 1.
        let alice_group = Group::join_from_welcome(&accepted.welcome_for_owner, &alice).unwrap();
        assert_eq!(alice_group.own_leaf_index().u32(), 1);
    }
}
