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
        let mut bob_send = accepted.session.own_participant_key().unwrap();
        let alice_recv_bob = alice_session
            .remote_participant_key(accepted.session.own_leaf_index)
            .unwrap();
        let bob_to_alice = b"hello from Bob through the full wire chain";
        let (ctr, ct) = bob_send.seal(aad, bob_to_alice).unwrap();
        let opened = alice_recv_bob.open(aad, &ct, ctr).unwrap();
        assert_eq!(opened.as_slice(), bob_to_alice.as_ref());

        let mut alice_send = alice_session.own_participant_key().unwrap();
        let bob_recv_alice = accepted
            .session
            .remote_participant_key(alice_session.own_leaf_index)
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
        use lakko_contacts::CallWelcomeError;

        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(CALL_WELCOME_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[0u8; 8]); // 8 < 32
        let err = dispatch_incoming_envelope(&wrapped).unwrap_err();
        assert_eq!(
            err,
            CallWelcomeError::TruncatedPayload { min: 32, got: 8 },
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
}
