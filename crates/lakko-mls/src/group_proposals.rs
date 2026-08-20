// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Proposal classification + the **one** ADR-0045 proposal-authorisation rule.
//!
//! [ADR-0045] replaces direct membership commits with proposals swept by a
//! designated committer: a member emits an MLS **Proposal** and exactly one
//! owner-or-admin turns the pending set into a single Commit. Proposals are not
//! epoch-advancing, so two members proposing concurrently compose instead of
//! forking the loser off the group (G-38 residual 1).
//!
//! The security consequence, and the reason this module exists, is stated in
//! ADR-0045 decision 3: before ADR-0045 a received proposal was **discarded
//! unexamined** (`ProcessedMessageContent::ProposalMessage(_) => Incoming::Proposal`),
//! which was safe *only* because Lakko stored no proposals — one that is never
//! stored can never be swept into a commit. Storing them removes that safety, so
//! every proposal must be judged against the ADR-0022 [`RoleTable`] by the role
//! of **whoever authored it**, not by the role of whoever commits it.
//!
//! # One rule, three call sites
//!
//! [`authorise`] is the whole per-proposal rule and the only place it is written
//! down. It is consumed at exactly three points in [`crate::group`], which is
//! what keeps them from drifting apart:
//!
//! 1. **The receipt gate** — [`crate::Group::process_incoming_verifying_roles`]
//!    stores an incoming proposal only if `authorise` accepts it, and drops it
//!    otherwise, so the pending store never holds anything unauthorised.
//! 2. **The sweep filter** — the committer's `CommitBuilder::build` closure
//!    re-runs `authorise` before including anything. `build` demands a filter,
//!    and the one ADR-0045 rejects is precisely OpenMLS's own `|_| true` (what
//!    `commit_to_pending_proposals` hardcodes), so this is not optional. It is
//!    the single choke point guaranteeing this device never commits an
//!    unauthorised proposal *however the store was populated* — and the store
//!    has three writers: the receipt gate, this crate's own `propose_*`, and
//!    `MlsGroup::load` restoring persisted entries.
//! 3. **The peer-verified commit gate** — [`crate::Group::process_incoming_verifying_roles`]
//!    runs `authorise` over **every** proposal a received Commit carries, judged
//!    against the pre-merge table. A malicious committer would not run (1) or
//!    (2), so this is what does not trust it.
//!
//! # Two corrections to ADR-0045 decision 3, found while building it
//!
//! * The ADR justifies (2) by "a proposal stored while its sender was an admin
//!   and swept after that sender was demoted". **That window does not exist.**
//!   OpenMLS empties the entire proposal store on every merged commit
//!   (`staged_commit.rs`, both merge arms), and the role table can only change
//!   *by* a merged commit — so the table can never move underneath a stored
//!   proposal. (2) is therefore defence in depth over the store's other
//!   writers, not a fix for a reachable demotion race. It is kept, and tested
//!   against a store primed by a path that bypassed (1).
//! * The ADR calls (1) "hygiene, not the boundary" and (3) "the boundary". For
//!   a **by-reference** proposal it is the other way round: a peer that dropped
//!   the proposal at (1) has nothing for OpenMLS's reference resolution to find,
//!   so the commit carrying it is refused with `InvalidCommit(MissingProposal)`
//!   before (3) is reached. (1) is a boundary, enforced by RFC 9420 itself.
//!   (3) remains necessary for the by-value arm — where it is ADR-0022's
//!   original committer rule — and for the proposal kinds ADR-0045 adds.
//!
//! # By-reference versus by-value
//!
//! A Commit carries each proposal either **by reference** (the proposal was
//! broadcast separately; the receiver looks it up in its own store and the
//! `QueuedProposal` keeps the **original proposer** as its sender) or **by
//! value** (inlined by the committer; the sender *is* the committer). Judging
//! `QueuedProposal::sender()` therefore gets both cases right with no branch:
//! by-reference is judged against the proposer, by-value against the committer —
//! and the by-value case is byte-for-byte the pre-ADR-0045 ADR-0022 rule, which
//! is why every direct-commit path keeps behaving exactly as it did.
//!
//! # The one rule that is not per-proposal
//!
//! [`owner_survives`] judges a commit as a whole: it must not leave the group
//! with an owner who is not a member of it. It cannot be folded into
//! [`authorise`], because neither way of reaching that state is visible to a
//! single-proposal check — a role table naming a key no leaf holds needs the
//! roster, and a transfer riding alongside the new owner's removal is two
//! individually-authorised proposals. It exists because ADR-0022's deferred
//! ownership transfer landed, and it is what keeps that feature from being able
//! to brick a group; see its own documentation for both cases in full.
//!
//! # A pending proposal must not mute the group
//!
//! Retaining proposals has one consequence ADR-0045 does not record: OpenMLS
//! refuses `MlsGroup::create_message` while its proposal store is non-empty, so
//! storing an incoming proposal would — untreated — take the whole group's chat
//! down from the moment one admin proposes until a committer sweeps, which
//! under ADR-0045's liveness model may be hours. That is handled entirely on
//! the send path; see [`crate::Group::create_application_message`] for the park
//! and the reasoning behind where the pending set lives while it happens.
//!
//! [ADR-0045]: ../../../docs/adr/0045-remote-commit-epoch-fork.md

use openmls::framing::Sender;
use openmls::group::QueuedProposal;
use openmls::messages::proposals::Proposal;

use crate::error::{Error, Result};
use crate::group::MemberRecord;
use crate::group_roles::{RoleTable, ROLE_EXTENSION_TYPE};

/// What a proposal asks the group to do, in Lakko terms.
///
/// This is both the classification a client renders ("Bob asked to remove
/// Charlie") and the input to [`authorise`] — deliberately the same type, so a
/// caller cannot describe a proposal one way and authorise it another. A
/// proposal type Lakko does not emit classifies as [`ProposalKind::Unsupported`]
/// and is always refused (fail-closed, invariant #4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProposalKind {
    /// Add a prospective member, carrying their Ed25519 signature public key
    /// (the stable identity handle the role table and the contact graph key on).
    Add {
        /// The prospective member's 32-byte Ed25519 signature public key.
        signature_key: Vec<u8>,
    },
    /// Remove the member sitting at `target_leaf`.
    Remove {
        /// The leaf index the proposal removes.
        target_leaf: u32,
    },
    /// The sender leaves the group — RFC 9420 `SelfRemove`, which structurally
    /// targets the sender's own leaf and can name no other.
    Leave,
    /// The sender rotates its own leaf node. Targets only the sender by RFC
    /// 9420, so it confers no authority over anyone else.
    Update,
    /// Rewrite the GroupContext extensions — in Lakko, the ADR-0022 role table.
    RoleChange,
    /// A proposal type Lakko never emits (PSK, `ReInit`, `ExternalInit`, a
    /// custom proposal, …). Always refused.
    Unsupported,
}

/// A proposal as the receive path saw it: who sent it, what it asks for, and
/// whether the ADR-0045 receipt gate retained it.
///
/// `stored == false` is the wire-visible form of a **refusal**. A proposal does
/// not advance the epoch, so refusing one changes no group state — dropping it
/// unstored *is* the refusal, and surfacing that as data rather than as an error
/// is what lets a client render ADR-0045 decision 5's terminal **refused** state
/// instead of logging an indistinguishable parse failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalRecord {
    /// The proposing member's leaf index, or `None` for a sender that is not a
    /// resolvable group member (always unauthorised — fail-closed).
    pub sender_leaf: Option<u32>,
    /// What the proposal asks for.
    pub kind: ProposalKind,
    /// `true` when the proposal was authorised and retained in the pending set,
    /// so a later sweep can commit it; `false` when it was dropped.
    pub stored: bool,
}

/// The author of a proposal as the gate sees them: the MLS-authenticated sender
/// leaf plus that leaf's signature public key, resolved from the **pre-merge**
/// roster. A sender that cannot be resolved to a member has no `Proposer` at
/// all, and [`authorise`] refuses every kind in that case.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Proposer<'a> {
    /// The sender's leaf index.
    pub leaf: u32,
    /// The sender's Ed25519 signature public key at that leaf.
    pub pubkey: &'a [u8],
}

/// Classify an OpenMLS [`Proposal`] into its Lakko [`ProposalKind`].
pub(crate) fn classify(proposal: &Proposal) -> ProposalKind {
    match proposal {
        Proposal::Add(add) => ProposalKind::Add {
            signature_key: add
                .key_package()
                .leaf_node()
                .signature_key()
                .as_slice()
                .to_vec(),
        },
        Proposal::Remove(remove) => ProposalKind::Remove {
            target_leaf: remove.removed().u32(),
        },
        Proposal::SelfRemove => ProposalKind::Leave,
        Proposal::Update(_) => ProposalKind::Update,
        Proposal::GroupContextExtensions(_) => ProposalKind::RoleChange,
        // Everything Lakko does not emit — PSK / ReInit / ExternalInit /
        // Custom, plus the draft-08 variants behind an OpenMLS feature flag.
        _ => ProposalKind::Unsupported,
    }
}

/// Resolve a leaf index to that leaf's signature public key in `members`.
pub(crate) fn pubkey_at_leaf(members: &[MemberRecord], leaf: u32) -> Option<&[u8]> {
    members
        .iter()
        .find(|m| m.leaf_index == leaf)
        .map(|m| m.signature_public_key.as_slice())
}

/// `true` when `leaf` currently holds the group owner's signature key.
///
/// A leaf that is not in `members` is **not** the owner — the same reading the
/// pre-ADR-0045 commit gate used, so an unknown leaf still falls through to the
/// owner-or-admin requirement below rather than being silently blessed.
fn leaf_is_owner(table: &RoleTable, members: &[MemberRecord], leaf: u32) -> bool {
    pubkey_at_leaf(members, leaf).is_some_and(|pk| table.is_owner(pk))
}

/// **The** ADR-0045 proposal-authorisation rule.
///
/// Decides whether `proposer` may ask the group for `kind`, judged against the
/// role table `table` **as it stands now** (pre-merge at the commit gate, per
/// ADR-0022 Finding 2 — a proposal is judged against the table it inherits,
/// never one it writes). Returns `Ok(())` when authorised; every other outcome
/// is an [`Error::Group`] naming the refusal.
///
/// The rule, in full:
///
/// | Ask | Who may |
/// |---|---|
/// | [`ProposalKind::Add`] | owner or admin |
/// | [`ProposalKind::Remove`] of the **owner's** leaf | nobody — the owner is unremovable |
/// | [`ProposalKind::Remove`] of the **proposer's own** leaf | anyone (this is leaving) |
/// | [`ProposalKind::Remove`] of any other leaf | owner or admin |
/// | [`ProposalKind::Leave`] by the **owner** | nobody — transfer first (see below) |
/// | [`ProposalKind::Leave`] by anyone else | anyone |
/// | [`ProposalKind::Update`] | anyone (targets only the sender's own leaf) |
/// | [`ProposalKind::RoleChange`] | the owner only |
/// | [`ProposalKind::Unsupported`] | nobody |
///
/// The `Add` / non-self `Remove` / `RoleChange` rows are ADR-0022's existing
/// committer rules, moved onto the proposal's author. The rest are new because
/// ADR-0045 makes them reachable for the first time:
///
/// * **Leaving.** ADR-0022 deleted its self-Remove branch as unreachable —
///   OpenMLS `CannotRemoveSelf` stops a committer committing the removal of its
///   own leaf — and recorded that a future sprint making "leaving" a controlled
///   action must add the branch back. ADR-0045 is that sprint: a member may now
///   *propose* its own removal (either as `SelfRemove`, or as a plain `Remove`
///   naming its own leaf) for an admin to commit.
/// * **The owner may not leave *while owner*.** ADR-0045 does not settle this;
///   it is decided here, fail-closed, for two reasons. First, consistency:
///   ADR-0022's owner-unremovable rule is unconditional and has no
///   self-exception, and a `SelfRemove` does **not** appear in
///   `StagedCommit::remove_proposals()` (proposal type `0x000a`), so without an
///   explicit refusal an owner-leaving commit would sail past the very check
///   that exists to keep the owner in the group. Second, consequence:
///   `set_group_roles` is owner-only, so an ownerless group can never appoint
///   another admin again — a permanent degradation.
///
///   This rule is **unchanged** by ownership transfer, and is the reason the
///   invariant survives it: the way out is
///   [`crate::Group::transfer_ownership`], after which the departing member is
///   no longer the owner and this arm simply does not apply to them. There is
///   no owner-leaves special case anywhere, and the group is never ownerless at
///   any instant — the transfer commit installs an owner who is a member of the
///   group it produces ([`owner_survives`]), and only then can the former owner
///   leave. The two steps cannot even be fused into one commit, because both
///   proposals are judged against the **pre-merge** table in which the
///   transferor is still the owner, so the `Leave` half is refused. Two rounds
///   is also the honest ordering: the transfer must be durable before its
///   author departs.
///
/// # Errors
///
/// [`Error::Group`] when `proposer` is absent (unresolvable sender) or the
/// proposer's role does not authorise `kind`.
pub(crate) fn authorise(
    table: &RoleTable,
    members: &[MemberRecord],
    proposer: Option<Proposer<'_>>,
    kind: &ProposalKind,
) -> Result<()> {
    let Some(proposer) = proposer else {
        return Err(Error::Group(
            "proposal refused: sender is not a resolvable group member".into(),
        ));
    };
    let is_owner = table.is_owner(proposer.pubkey);
    let can_administer = table.can_administer(proposer.pubkey);

    match kind {
        ProposalKind::Add { .. } => {
            if !can_administer {
                return Err(Error::Group(
                    "add proposal refused: proposer is not owner or admin".into(),
                ));
            }
        },
        ProposalKind::Remove { target_leaf } => {
            if leaf_is_owner(table, members, *target_leaf) {
                return Err(Error::Group(
                    "removal proposal refused: the group owner cannot be removed".into(),
                ));
            }
            // Removing exactly your own leaf is leaving, and needs no role.
            if *target_leaf != proposer.leaf && !can_administer {
                return Err(Error::Group(
                    "removal proposal refused: proposer is not owner or admin".into(),
                ));
            }
        },
        ProposalKind::Leave => {
            if is_owner {
                return Err(Error::Group(
                    "self-removal proposal refused: the group owner cannot leave".into(),
                ));
            }
        },
        // An Update replaces only the sender's own leaf node (RFC 9420 §12.1.2),
        // so it confers no authority over any other member.
        ProposalKind::Update => {},
        ProposalKind::RoleChange => {
            if !is_owner {
                return Err(Error::Group(
                    "role-table change proposal refused: proposer is not the owner".into(),
                ));
            }
        },
        ProposalKind::Unsupported => {
            return Err(Error::Group(
                "proposal refused: unsupported proposal type".into(),
            ));
        },
    }
    Ok(())
}

/// What a set of proposals would do to the group's membership and to its role
/// table, taken **together** as one commit.
///
/// Every other rule in this module judges one proposal in isolation, which is
/// right for authorisation — who may ask for a thing does not depend on what
/// else is being asked. [`owner_survives`] is the one rule that cannot be
/// written that way, so it is given the whole commit's effect summary instead.
///
/// Owned rather than borrowed so a caller can compute it, drop the borrow on the
/// proposal store, and still consult it — which is what
/// [`crate::Group::commit_pending_proposals`] needs in order to decide its sweep
/// filter before it takes `commit_builder()`.
#[derive(Debug, Default)]
pub(crate) struct CommitEffects {
    /// The owner named by each role-table rewrite the commit carries. Normally
    /// zero or one — RFC 9420 admits at most one `GroupContextExtensions`
    /// proposal per commit — but the check is written for a list so a
    /// hypothetical second one cannot slip past by being ignored.
    proposed_owners: Vec<Vec<u8>>,
    /// `true` when the commit rewrites the GroupContext but the result carries
    /// no readable `0xFF02` role table. In a role-enabled group that means the
    /// commit would **delete** the group's authority structure, silently
    /// reverting it to the open/legacy shape where any member may add or
    /// remove. Refused rather than reasoned about.
    role_table_would_vanish: bool,
    /// Leaves the commit removes — including the sender's own leaf for a
    /// `SelfRemove`, which carries no target of its own.
    removed_leaves: Vec<u32>,
    /// Signature keys the commit adds.
    added_keys: Vec<Vec<u8>>,
}

/// Summarise what `proposals` would do if committed together.
pub(crate) fn commit_effects<'a>(
    proposals: impl Iterator<Item = &'a QueuedProposal>,
) -> CommitEffects {
    let mut effects = CommitEffects::default();
    for qp in proposals {
        match classify(qp.proposal()) {
            ProposalKind::Add { signature_key } => effects.added_keys.push(signature_key),
            ProposalKind::Remove { target_leaf } => effects.removed_leaves.push(target_leaf),
            ProposalKind::Leave => {
                // `SelfRemove` names no target: the removed leaf is the sender's
                // own. An unresolvable sender removes nobody.
                if let Sender::Member(idx) = qp.sender() {
                    effects.removed_leaves.push(idx.u32());
                }
            },
            ProposalKind::RoleChange => {
                // `classify` deliberately discards the payload (the public
                // `ProposalKind` is an FFI-facing type and carries no table), so
                // the one field this rule needs is read from the raw proposal.
                match role_table_of(qp.proposal()) {
                    Some(table) => effects.proposed_owners.push(table.owner().to_vec()),
                    None => effects.role_table_would_vanish = true,
                }
            },
            ProposalKind::Update | ProposalKind::Unsupported => {},
        }
    }
    effects
}

/// The role table a `GroupContextExtensions` proposal would install, or `None`
/// when it carries no readable `0xFF02` body.
fn role_table_of(proposal: &Proposal) -> Option<RoleTable> {
    let Proposal::GroupContextExtensions(gce) = proposal else {
        return None;
    };
    let body = gce.extensions().unknown(ROLE_EXTENSION_TYPE)?;
    RoleTable::decode(&body.0).ok()
}

/// **The group must never be left with an owner who is not in it.**
///
/// The rule the whole ownership-transfer feature rests on, and the only one that
/// has to see a commit rather than a proposal. It is checked against the roster
/// the commit *produces* — pre-merge members, minus everyone it removes, plus
/// everyone it adds — because that is when the new table takes effect.
///
/// Two ways to reach an unowned group, both closed here:
///
/// * **Transfer to a non-member.** A role table names 32-byte keys, so nothing
///   in the codec stops an owner naming a key that is in no leaf. Nobody could
///   then ever change the table again, because a role change is owner-only, and
///   no removal could repair it, because the owner's leaf is unremovable and
///   there is no such leaf anyway. Permanent, and reachable today by an owner
///   calling `set_group_roles` with a hand-built table — this check refuses that
///   commit on **every** peer, so it is a strict improvement independent of
///   whether anyone uses the named transfer API.
/// * **Transfer to a member the same commit removes.** ADR-0045 composes N
///   intents into one commit, so "hand the group to Charlie" and "remove
///   Charlie" can ride together. Each is individually authorised — the owner may
///   rewrite the table, an admin may remove a plain member — so no per-proposal
///   rule can see it. The post-commit roster does.
///
/// A commit that rewrites the GroupContext without leaving a readable role table
/// behind is refused for the same reason: it would delete the group's authority
/// structure rather than change it.
///
/// # Errors
///
/// [`Error::Group`] when the commit would leave the group's owner outside it, or
/// would erase the role table. A commit carrying no role change is always
/// authorised by this rule.
pub(crate) fn owner_survives(effects: &CommitEffects, members: &[MemberRecord]) -> Result<()> {
    if effects.role_table_would_vanish {
        return Err(Error::Group(
            "commit refused: it would remove this group's role table".into(),
        ));
    }
    if effects.proposed_owners.is_empty() {
        return Ok(());
    }
    let survives = |key: &[u8]| {
        let stays = members.iter().any(|m| {
            m.signature_public_key == key && !effects.removed_leaves.contains(&m.leaf_index)
        });
        stays || effects.added_keys.iter().any(|k| k == key)
    };
    for owner in &effects.proposed_owners {
        if !survives(owner) {
            return Err(Error::Group(
                "role-table change refused: the named owner would not be a member of the group"
                    .into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(b: u8) -> [u8; 32] {
        [b; 32]
    }

    /// owner = 1, admin = 2, plain member = 3, at leaves 0 / 1 / 2.
    fn fixture() -> (RoleTable, Vec<MemberRecord>) {
        let table = RoleTable::new(pk(1), vec![pk(2)]);
        let members = vec![
            MemberRecord {
                leaf_index: 0,
                signature_public_key: pk(1).to_vec(),
            },
            MemberRecord {
                leaf_index: 1,
                signature_public_key: pk(2).to_vec(),
            },
            MemberRecord {
                leaf_index: 2,
                signature_public_key: pk(3).to_vec(),
            },
        ];
        (table, members)
    }

    fn proposer(leaf: u32, key: &[u8]) -> Proposer<'_> {
        Proposer { leaf, pubkey: key }
    }

    fn add_kind() -> ProposalKind {
        ProposalKind::Add {
            signature_key: pk(9).to_vec(),
        }
    }

    #[test]
    fn add_needs_owner_or_admin() {
        let (t, m) = fixture();
        let owner = pk(1);
        let admin = pk(2);
        let member = pk(3);
        assert!(authorise(&t, &m, Some(proposer(0, &owner)), &add_kind()).is_ok());
        assert!(authorise(&t, &m, Some(proposer(1, &admin)), &add_kind()).is_ok());
        assert!(authorise(&t, &m, Some(proposer(2, &member)), &add_kind()).is_err());
    }

    #[test]
    fn removing_a_member_needs_owner_or_admin() {
        let (t, m) = fixture();
        let admin = pk(2);
        let member = pk(3);
        let remove_member = ProposalKind::Remove { target_leaf: 2 };
        assert!(authorise(&t, &m, Some(proposer(1, &admin)), &remove_member).is_ok());
        // charlie (plain member) may not remove someone else...
        let remove_admin = ProposalKind::Remove { target_leaf: 1 };
        assert!(authorise(&t, &m, Some(proposer(2, &member)), &remove_admin).is_err());
    }

    #[test]
    fn removing_your_own_leaf_is_leaving_and_needs_no_role() {
        let (t, m) = fixture();
        let member = pk(3);
        assert!(
            authorise(
                &t,
                &m,
                Some(proposer(2, &member)),
                &ProposalKind::Remove { target_leaf: 2 }
            )
            .is_ok(),
            "a plain member must be able to propose its own removal",
        );
        assert!(authorise(&t, &m, Some(proposer(2, &member)), &ProposalKind::Leave).is_ok());
    }

    #[test]
    fn the_owner_can_never_be_removed_by_anyone_including_itself() {
        let (t, m) = fixture();
        let owner = pk(1);
        let admin = pk(2);
        let remove_owner = ProposalKind::Remove { target_leaf: 0 };
        assert!(authorise(&t, &m, Some(proposer(1, &admin)), &remove_owner).is_err());
        assert!(authorise(&t, &m, Some(proposer(0, &owner)), &remove_owner).is_err());
        assert!(
            authorise(&t, &m, Some(proposer(0, &owner)), &ProposalKind::Leave).is_err(),
            "the owner must not be able to leave via SelfRemove either",
        );
    }

    #[test]
    fn role_change_is_owner_only() {
        let (t, m) = fixture();
        let owner = pk(1);
        let admin = pk(2);
        assert!(authorise(&t, &m, Some(proposer(0, &owner)), &ProposalKind::RoleChange).is_ok());
        assert!(authorise(&t, &m, Some(proposer(1, &admin)), &ProposalKind::RoleChange).is_err());
    }

    #[test]
    fn update_is_open_to_any_member() {
        let (t, m) = fixture();
        let member = pk(3);
        assert!(authorise(&t, &m, Some(proposer(2, &member)), &ProposalKind::Update).is_ok());
    }

    #[test]
    fn unsupported_and_unresolvable_are_refused() {
        let (t, m) = fixture();
        let owner = pk(1);
        assert!(authorise(
            &t,
            &m,
            Some(proposer(0, &owner)),
            &ProposalKind::Unsupported
        )
        .is_err());
        // Even the owner cannot smuggle an unsupported proposal through.
        assert!(authorise(&t, &m, None, &add_kind()).is_err());
        assert!(authorise(&t, &m, None, &ProposalKind::Update).is_err());
        assert!(authorise(&t, &m, None, &ProposalKind::Leave).is_err());
    }

    #[test]
    fn an_unknown_leaf_is_not_the_owner_but_still_needs_admin_to_remove() {
        let (t, m) = fixture();
        let member = pk(3);
        let admin = pk(2);
        let remove_ghost = ProposalKind::Remove { target_leaf: 99 };
        assert!(authorise(&t, &m, Some(proposer(2, &member)), &remove_ghost).is_err());
        assert!(authorise(&t, &m, Some(proposer(1, &admin)), &remove_ghost).is_ok());
    }
}
