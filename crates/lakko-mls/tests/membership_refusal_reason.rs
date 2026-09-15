// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Machine-readable membership refusals — property tests for
//! [`lakko_mls::RefusalReason`] (ADR-0045 §9.1 item 1 / §12 item 6).
//!
//! **What these tests assert, and what they deliberately do not.** Every
//! assertion is on the *reason*, never on the message text. A reworded refusal
//! must not break a test here, and a refusal that reports the *wrong* reason
//! must not pass one — that pair is the whole point of the type, so it is the
//! whole point of the suite. The one place prose is touched is
//! [`every_reason_still_carries_its_prose`], which asserts only that a detail
//! exists and is non-empty, because the prose is kept for logs.
//!
//! The scene is the same 3-member role-enabled group ADR-0022's and ADR-0045's
//! suites use — owner = alice, admin = bob, member = charlie — so all three can
//! be read side by side.

use lakko_mls::{
    group, key_package, Error, Group, Identity, Incoming, LeafNodeIndex, OutgoingOwner,
    RefusalDisposition, RefusalReason, RoleTable,
};

fn wire(msg: openmls::framing::MlsMessageOut) -> Vec<u8> {
    group::encode_message(&msg).expect("encode message")
}

fn kp_wire(id: &Identity) -> Vec<u8> {
    key_package::serialize(&id.create_key_package().expect("key package")).expect("serialize kp")
}

fn pk32(id: &Identity) -> [u8; 32] {
    id.signature_public_key()
        .try_into()
        .expect("Ed25519 signature key is 32 bytes")
}

fn leaf_of(g: &Group, pubkey: &[u8]) -> LeafNodeIndex {
    let idx = g
        .members()
        .iter()
        .find(|m| m.signature_public_key == pubkey)
        .expect("member present")
        .leaf_index;
    LeafNodeIndex::new(idx)
}

/// The error a call produced, or a panic naming what it did instead.
///
/// `expect_err` needs `T: Debug` and several success types here are opaque
/// handles, so the unwrap is written once rather than at each site.
#[track_caller]
fn err_of<T>(r: Result<T, Error>, what: &str) -> Error {
    match r {
        Ok(_) => panic!("{what} must be refused, but it succeeded"),
        Err(e) => e,
    }
}

/// The reason an [`Error`] reports, or a panic naming what it reported instead.
///
/// Written once so every test below reads as an assertion about a reason and
/// never about a string.
#[track_caller]
fn reason_of(err: &Error, what: &str) -> RefusalReason {
    err.refusal_reason()
        .unwrap_or_else(|| panic!("{what} must report a typed refusal reason, got {err:?}"))
}

struct Scene {
    alice: Identity,
    bob: Identity,
    charlie: Identity,
    alice_g: Group,
    bob_g: Group,
    charlie_g: Group,
}

/// owner = alice, admin = bob, member = charlie.
fn scene(tag: u8) -> Scene {
    let alice = Identity::from_seed(&[0xA0 ^ tag; 64]).expect("alice");
    let bob = Identity::from_seed(&[0xB0 ^ tag; 64]).expect("bob");
    let charlie = Identity::from_seed(&[0xC0 ^ tag; 64]).expect("charlie");

    let mut alice_g = Group::create(&alice).expect("alice creates group");

    let add_bob = alice_g
        .add_member_from_wire(&alice, &kp_wire(&bob))
        .expect("alice adds bob");
    let mut bob_g = Group::join_from_welcome(&wire(add_bob.welcome), &bob).expect("bob joins");

    let add_c = alice_g
        .add_member_from_wire(&alice, &kp_wire(&charlie))
        .expect("alice adds charlie");
    bob_g
        .process_incoming_verifying_roles(&bob, &wire(add_c.commit))
        .expect("bob applies owner's add-charlie");
    let mut charlie_g =
        Group::join_from_welcome(&wire(add_c.welcome), &charlie).expect("charlie joins");

    let roles = RoleTable::new(pk32(&alice), vec![pk32(&bob)]);
    let promote = alice_g
        .set_group_roles(&alice, &roles)
        .expect("alice promotes bob");
    bob_g
        .process_incoming_verifying_roles(&bob, &promote)
        .expect("bob applies promote");
    charlie_g
        .process_incoming_verifying_roles(&charlie, &promote)
        .expect("charlie applies promote");

    Scene {
        alice,
        bob,
        charlie,
        alice_g,
        bob_g,
        charlie_g,
    }
}

// ============================================================================
// One reason per refusal — the six §6 authorisation rows
// ============================================================================

#[test]
fn a_plain_member_adding_reports_add_not_owner_or_admin() {
    let mut s = scene(0x01);
    let dave = Identity::from_seed(&[0x11u8; 64]).expect("dave");
    let err = s
        .charlie_g
        .propose_add_member_from_wire(&s.charlie, &kp_wire(&dave))
        .expect_err("a plain member may not add");
    assert_eq!(
        reason_of(&err, "a member's add"),
        RefusalReason::AddNotOwnerOrAdmin,
    );
}

#[test]
fn a_plain_member_removing_someone_else_reports_remove_not_owner_or_admin() {
    let mut s = scene(0x02);
    let bob_leaf = leaf_of(&s.charlie_g, s.bob.signature_public_key());
    let err = s
        .charlie_g
        .propose_remove_member(&s.charlie, bob_leaf)
        .expect_err("a plain member may not remove someone else");
    assert_eq!(
        reason_of(&err, "a member's removal of another"),
        RefusalReason::RemoveNotOwnerOrAdmin,
    );
}

#[test]
fn removing_the_owner_reports_owner_unremovable() {
    let mut s = scene(0x03);
    let alice_leaf = leaf_of(&s.bob_g, s.alice.signature_public_key());
    let err = s
        .bob_g
        .propose_remove_member(&s.bob, alice_leaf)
        .expect_err("the owner may not be removed");
    assert_eq!(
        reason_of(&err, "an admin's removal of the owner"),
        RefusalReason::OwnerUnremovable,
    );
}

/// The owner's *own* removal is refused by the same rule, and must report the
/// same reason — not the self-removal one. `authorise` reaches the owner arm
/// first, and a client showing "you cannot leave" to an owner who asked to
/// remove somebody would be wrong copy for the right refusal.
#[test]
fn the_owner_removing_its_own_leaf_still_reports_owner_unremovable() {
    let mut s = scene(0x04);
    let alice_leaf = leaf_of(&s.alice_g, s.alice.signature_public_key());
    let err = s
        .alice_g
        .propose_remove_member(&s.alice, alice_leaf)
        .expect_err("the owner may not remove itself either");
    assert_eq!(
        reason_of(&err, "the owner's own removal"),
        RefusalReason::OwnerUnremovable,
    );
}

/// Leaving is the one authorised self-targeted removal, so it must report **no**
/// refusal at all. (`RefusalReason::OwnerCannotLeave` covers the `SelfRemove`
/// arm, which no public API emits — it is unit-tested against `authorise`
/// directly, beside the rule.)
#[test]
fn a_plain_member_may_propose_its_own_removal() {
    let mut s = scene(0x05);
    let charlie_leaf = leaf_of(&s.charlie_g, s.charlie.signature_public_key());
    s.charlie_g
        .propose_remove_member(&s.charlie, charlie_leaf)
        .expect("a plain member may propose its own removal");
}

#[test]
fn a_non_owner_changing_the_role_table_reports_not_owner() {
    let mut s = scene(0x06);
    let table = RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob), pk32(&s.charlie)]);
    let err = s
        .bob_g
        .propose_group_roles(&s.bob, &table)
        .expect_err("an admin may not change the role table");
    assert_eq!(
        reason_of(&err, "an admin's role change"),
        RefusalReason::NotOwner,
    );

    // The direct path is the same rule at a second site, and must agree.
    let err = s
        .bob_g
        .set_group_roles(&s.bob, &table)
        .expect_err("an admin may not change the role table directly either");
    assert_eq!(
        reason_of(&err, "an admin's direct role change"),
        RefusalReason::NotOwner,
    );

    // So is ownership transfer — it *is* a role change.
    let err = s
        .bob_g
        .propose_ownership_transfer(&s.bob, &pk32(&s.charlie), OutgoingOwner::Member)
        .expect_err("an admin may not transfer ownership");
    assert_eq!(
        reason_of(&err, "an admin's ownership transfer"),
        RefusalReason::NotOwner,
    );
}

// ============================================================================
// The §6 fail-closed fallback row — three distinct reasons behind one string
// ============================================================================

#[test]
fn a_group_with_no_role_table_reports_no_role_table() {
    let alice = Identity::from_seed(&[0x21u8; 64]).expect("alice");
    let bob = Identity::from_seed(&[0x22u8; 64]).expect("bob");
    // `create_with_id` seeds no role table (a call group / PublisherGroup).
    let mut g = Group::create_with_id(&alice, b"no-role-table-group").expect("create");
    let err = g
        .propose_add_member_from_wire(&alice, &kp_wire(&bob))
        .expect_err("a table-less group has no committer");
    assert_eq!(
        reason_of(&err, "a proposal into a table-less group"),
        RefusalReason::NoRoleTable,
    );
}

#[test]
fn a_named_owner_who_is_not_a_member_reports_owner_not_a_member() {
    let mut s = scene(0x07);
    let stranger = Identity::from_seed(&[0x31u8; 64]).expect("stranger");
    let table = RoleTable::new(pk32(&stranger), vec![pk32(&s.bob)]);
    let err = s
        .alice_g
        .propose_group_roles(&s.alice, &table)
        .expect_err("an owner outside the group is unenforceable");
    assert_eq!(
        reason_of(&err, "a role table naming a non-member owner"),
        RefusalReason::OwnerNotAMember,
    );

    let err = s
        .alice_g
        .propose_ownership_transfer(&s.alice, &pk32(&stranger), OutgoingOwner::Admin)
        .expect_err("ownership cannot go to a non-member");
    assert_eq!(
        reason_of(&err, "a transfer to a non-member"),
        RefusalReason::OwnerNotAMember,
    );
}

// ============================================================================
// The availability class — the seventh §6 string and its siblings
// ============================================================================

#[test]
fn a_second_role_change_reports_role_change_pending() {
    let mut s = scene(0x08);
    let promote_charlie = RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob), pk32(&s.charlie)]);
    s.alice_g
        .propose_group_roles(&s.alice, &promote_charlie)
        .expect("the owner's first role change");

    let demote_bob = RoleTable::new(pk32(&s.alice), vec![]);
    let err = s
        .alice_g
        .propose_group_roles(&s.alice, &demote_bob)
        .expect_err("only one role change may be pending");
    assert_eq!(
        reason_of(&err, "a second role change"),
        RefusalReason::RoleChangePending,
    );
}

#[test]
fn a_direct_commit_while_proposals_pend_reports_proposals_pending() {
    let mut s = scene(0x09);
    let dave = Identity::from_seed(&[0x41u8; 64]).expect("dave");
    s.alice_g
        .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("the owner proposes an add");

    let eve = Identity::from_seed(&[0x42u8; 64]).expect("eve");
    let err = err_of(
        s.alice_g.add_member_from_wire(&s.alice, &kp_wire(&eve)),
        "a direct add while proposals pend",
    );
    assert_eq!(
        reason_of(&err, "a direct add while proposals pend"),
        RefusalReason::ProposalsPending,
    );
}

#[test]
fn a_non_committer_sweeping_reports_not_a_committer() {
    let mut s = scene(0x0A);
    let dave = Identity::from_seed(&[0x51u8; 64]).expect("dave");
    let proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &proposal)
        .expect("charlie retains the admin's proposal");

    let err = err_of(
        s.charlie_g.stage_pending_proposals(&s.charlie),
        "a plain member's sweep",
    );
    assert_eq!(
        reason_of(&err, "a member's sweep"),
        RefusalReason::NotACommitter,
    );
}

/// The pending-set cap. §7.1 names it as needing typed copy and §6's table
/// does not carry it — a fallback saying "This change could not be requested"
/// would be wrong here, because waiting **is** the answer.
#[test]
fn filling_the_pending_set_reports_pending_set_full() {
    let mut s = scene(0x0D);
    let dave = Identity::from_seed(&[0x91u8; 64]).expect("dave");
    // One identity, many fresh KeyPackages: each proposal hashes to its own
    // `ProposalRef`, so nothing de-duplicates and the set fills. The cap is
    // `pub(crate)`, so the loop finds it rather than restating it — and the
    // bound is generous enough that never reaching it is itself a failure.
    let mut refusal = None;
    for _ in 0..512 {
        match s
            .alice_g
            .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
        {
            Ok(_) => {},
            Err(e) => {
                refusal = Some(e);
                break;
            },
        }
    }
    let err = refusal.expect("the pending set must eventually refuse an add");
    assert_eq!(
        reason_of(&err, "an add past the cap"),
        RefusalReason::PendingSetFull,
    );
}

#[test]
fn a_second_stage_reports_sweep_already_staged() {
    let mut s = scene(0x0B);
    let dave = Identity::from_seed(&[0x61u8; 64]).expect("dave");
    s.alice_g
        .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("the owner proposes");
    s.alice_g
        .stage_pending_proposals(&s.alice)
        .expect("the first stage");

    let err = err_of(
        s.alice_g.stage_pending_proposals(&s.alice),
        "a second stage",
    );
    assert_eq!(
        reason_of(&err, "a second stage"),
        RefusalReason::SweepAlreadyStaged,
    );
}

// ============================================================================
// The axis that drives whether the UI offers a retry
// ============================================================================

/// **The maintainer's rule: retryable exactly when a bounded queue was full.**
///
/// Asserted as a literal partition rather than derived from `disposition`, so
/// moving a reason between dispositions has to be stated here too. This is the
/// test that fails when someone quietly widens what a retry is offered for.
#[test]
fn the_three_dispositions_partition_the_reasons_exactly_like_this() {
    let of = |d: RefusalDisposition| -> Vec<RefusalReason> {
        RefusalReason::ALL
            .into_iter()
            .filter(|r| r.disposition() == d)
            .collect()
    };

    assert_eq!(
        of(RefusalDisposition::RetryLater),
        vec![
            RefusalReason::RoleChangePending,
            RefusalReason::PendingSetFull,
        ],
        "only a full bounded queue is retryable — the pending-proposal set, and \
         the single-GCE-per-commit slot, which is a queue of capacity one",
    );
    assert_eq!(
        of(RefusalDisposition::DoNotRetry),
        vec![
            RefusalReason::LeafUpdateRefused,
            RefusalReason::UnsupportedProposal,
            RefusalReason::NoRoleTable,
            RefusalReason::OwnerNotAMember,
            RefusalReason::RoleTableWouldVanish,
            RefusalReason::NoSuchJoinRequest,
            RefusalReason::JoinerAlreadyAMember,
            RefusalReason::ProposalsPending,
            RefusalReason::SweepAlreadyStaged,
            RefusalReason::DeviceLimitReached,
        ],
        "a refusal about the request's own state is never retryable, however \
         much the group's pending work drains",
    );
    assert_eq!(
        of(RefusalDisposition::NotPermitted),
        vec![
            RefusalReason::NotAMember,
            RefusalReason::AddNotOwnerOrAdmin,
            RefusalReason::RemoveNotOwnerOrAdmin,
            RefusalReason::OwnerUnremovable,
            RefusalReason::OwnerCannotLeave,
            RefusalReason::NotOwner,
            RefusalReason::NotACommitter,
            RefusalReason::DeviceBindingInvalid,
        ],
        "an authority answer offers no action at all",
    );
}

/// `retry_may_succeed` is exactly `RetryLater`, and in particular is **not**
/// the complement of `NotPermitted` — the middle disposition is the one a
/// two-valued axis would have got wrong.
#[test]
fn retry_may_succeed_is_narrower_than_not_being_forbidden() {
    for reason in RefusalReason::ALL {
        assert_eq!(
            reason.retry_may_succeed(),
            reason.disposition() == RefusalDisposition::RetryLater,
            "{reason:?}",
        );
    }
    assert!(
        !RefusalReason::SweepAlreadyStaged.retry_may_succeed(),
        "a staged sweep is not forbidden, and is still not retryable: the \
         caller must reach the outstanding verdict first",
    );
    assert!(
        !RefusalReason::OwnerNotAMember.retry_may_succeed(),
        "a role table naming a non-member owner is refused the same way \
         however long the caller waits",
    );
}

/// Codes are the wasm surface's discriminator and are therefore wire-visible.
/// They must be unique and must not drift.
#[test]
fn every_reason_has_a_distinct_stable_code() {
    let mut codes: Vec<&str> = RefusalReason::ALL.iter().map(|r| r.code()).collect();
    let total = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), total, "refusal codes must be unique");
    assert_eq!(
        RefusalReason::AddNotOwnerOrAdmin.code(),
        "addNotOwnerOrAdmin",
    );
    assert_eq!(RefusalReason::RoleChangePending.code(), "roleChangePending",);
}

// ============================================================================
// The prose is kept
// ============================================================================

/// The typed reason is *additional*. A developer reading a log or a stack must
/// still get the sentence, so no refusal may become a bare code.
#[test]
fn every_reason_still_carries_its_prose() {
    let mut s = scene(0x0C);
    let dave = Identity::from_seed(&[0x71u8; 64]).expect("dave");
    let err = s
        .charlie_g
        .propose_add_member_from_wire(&s.charlie, &kp_wire(&dave))
        .expect_err("a plain member may not add");
    let text = format!("{err}");
    assert!(
        text.len() > 20 && text.contains("owner"),
        "the refusal must still read as a sentence, got {text:?}",
    );
}

/// A failure that is not a membership refusal reports **no** reason, so a
/// client cannot mistake an OpenMLS or codec failure for a permission answer.
#[test]
fn a_non_membership_failure_reports_no_reason() {
    let alice = Identity::from_seed(&[0x81u8; 64]).expect("alice");
    let mut g = Group::create(&alice).expect("create");
    let err = g
        .propose_add_member_from_wire(&alice, b"not a key package")
        .expect_err("garbage is not a KeyPackage");
    assert!(
        err.refusal_reason().is_none(),
        "a wire-format failure is not a membership refusal, got {err:?}",
    );
}

// ============================================================================
// The stranger quarantine — the drop must not be silent
// ============================================================================

/// A retained request says so, and an approve on it works.
#[test]
fn a_retained_external_join_reports_retained_and_can_be_approved() {
    let mut s = scene(0x0E);
    let dave = Identity::from_seed(&[0xA1u8; 64]).expect("dave");
    let request = dave
        .create_external_join_proposal(&s.alice_g.group_id(), s.alice_g.epoch())
        .expect("dave asks to join");

    let seen = s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &request)
        .expect("the owner sees the request");
    let Incoming::ExternalJoinProposal {
        joiner_signature_key,
        retained,
    } = seen
    else {
        panic!("expected an external-join request");
    };
    assert!(retained, "the quarantine had room, so it was kept");
    s.alice_g
        .approve_external_join(&s.alice, &joiner_signature_key)
        .expect("an approve on a retained request works");
}

/// **The gap this closes.** A request arriving while the quarantine is full is
/// still surfaced — a stranger must not be able to make a member's receive loop
/// error — but it is **not** retained, and `retained == false` is the only
/// thing that says so. Without it the admin sees a pending request, taps
/// Approve, and gets a refusal for an entry that never existed.
#[test]
fn an_external_join_dropped_by_a_full_quarantine_says_it_was_not_retained() {
    let mut s = scene(0x0F);

    // Fill the quarantine. The cap is `pub(crate)`, so the loop finds it.
    let mut filled = 0usize;
    let mut overflow_key = None;
    for i in 0..512u32 {
        let mut seed = [0xB2u8; 64];
        seed[0..4].copy_from_slice(&i.to_be_bytes());
        let stranger = Identity::from_seed(&seed).expect("stranger");
        let request = stranger
            .create_external_join_proposal(&s.alice_g.group_id(), s.alice_g.epoch())
            .expect("stranger asks");
        let Incoming::ExternalJoinProposal {
            joiner_signature_key,
            retained,
        } = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, &request)
            .expect("the owner sees the request")
        else {
            panic!("expected an external-join request");
        };
        if retained {
            filled += 1;
        } else {
            overflow_key = Some(joiner_signature_key);
            break;
        }
    }
    let overflow_key = overflow_key.expect("the quarantine must eventually refuse to retain");
    assert!(filled > 0, "some requests must have been retained first");
    assert_eq!(
        s.alice_g.pending_external_joins().len(),
        filled,
        "an unretained request must not appear in the pending list either",
    );

    // And the follow-through: approving it is refused with a reason, not with
    // an unclassifiable sentence, so a client that ignored `retained` still
    // learns what happened rather than showing a generic failure.
    let err = err_of(
        s.alice_g.approve_external_join(&s.alice, &overflow_key),
        "approving a request that was never retained",
    );
    assert_eq!(
        reason_of(&err, "approving a dropped request"),
        RefusalReason::NoSuchJoinRequest,
    );
    assert_eq!(
        err.refusal_reason().map(RefusalReason::disposition),
        Some(RefusalDisposition::DoNotRetry),
        "a stale approve must not be offered as a retry",
    );
}

/// Approving a joiner who is already in the group is its own reason, not the
/// not-found one — the two lead to different copy ("nothing to do" against
/// "that request is gone").
#[test]
fn approving_an_existing_member_reports_joiner_already_a_member() {
    let mut s = scene(0x10);
    let bob_key = s.bob.signature_public_key().to_vec();
    let err = err_of(
        s.alice_g.approve_external_join(&s.alice, &bob_key),
        "approving a joiner who is already a member",
    );
    assert_eq!(
        reason_of(&err, "approving an existing member"),
        RefusalReason::JoinerAlreadyAMember,
    );
}
