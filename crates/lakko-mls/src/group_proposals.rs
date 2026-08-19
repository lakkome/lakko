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
//! [`authorise`] is the whole rule and the only place it is written down. It is
//! consumed at exactly three points in [`crate::group`], which is what keeps
//! them from drifting apart:
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
//! [ADR-0045]: ../../../docs/adr/0045-remote-commit-epoch-fork.md

use openmls::messages::proposals::Proposal;

use crate::error::{Error, Result};
use crate::group::MemberRecord;
use crate::group_roles::RoleTable;

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
/// | [`ProposalKind::Leave`] by the **owner** | nobody (see below) |
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
/// * **The owner may not leave.** ADR-0045 does not settle this; it is decided
///   here, fail-closed, for two reasons. First, consistency: ADR-0022's
///   owner-unremovable rule is unconditional and has no self-exception, and a
///   `SelfRemove` does **not** appear in `StagedCommit::remove_proposals()`
///   (proposal type `0x000a`), so without an explicit refusal an owner-leaving
///   commit would sail past the very check that exists to keep the owner in the
///   group. Second, consequence: `set_group_roles` is owner-only, so an
///   ownerless group can never appoint another admin again — a permanent
///   degradation, where refusing is merely an inconvenience until ADR-0022's
///   deferred ownership-transfer follow-up lands.
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
