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
//! # What "authorise" is asked, and what it is not
//!
//! The rule answers *may this member ask the group for this?* — but two of the
//! six kinds are refused for **everyone**, and neither refusal is a permission
//! judgement. [`ProposalKind::Unsupported`] is refused because Lakko cannot say
//! what it would do; [`ProposalKind::Update`] is refused because of what it
//! *would* do — it can rotate the very signature key [`RoleTable`] is keyed on,
//! which no per-proposal permission check could make safe, and it is the one
//! kind whose `ProposalRef` is fresh on every emission, which is what let a
//! plain member flood the pending set. Both are recorded in full on
//! [`authorise`]. This is the reason `authorise` returns a **named** error per
//! refusal rather than a bare `bool`: the two facts a caller needs — *who may*
//! and *what may* — arrive through the same door and must stay tellable apart.
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
use crate::group_kind::{GroupKind, KindState};
use crate::group_roles::{RoleTable, ROLE_EXTENSION_TYPE};
use crate::refusal::RefusalReason;

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
        /// The prospective member's 32-byte Ed25519 **leaf** signature key.
        signature_key: Vec<u8>,
        /// The account the leaf belongs to, resolved from its credential
        /// (ADR-0050): the device binding's account key, or the leaf key itself
        /// for an unbound Phase 1 leaf. `None` when the credential carries a
        /// binding that does not verify for this leaf — refused everywhere.
        account_key: Option<Vec<u8>>,
    },
    /// Remove the member sitting at `target_leaf`.
    Remove {
        /// The leaf index the proposal removes.
        target_leaf: u32,
    },
    /// The sender leaves the group — RFC 9420 `SelfRemove`, which structurally
    /// targets the sender's own leaf and can name no other.
    Leave,
    /// The sender rotates its own leaf node. **Always refused** — see
    /// [`authorise`] for why "targets only the sender" is not the same as
    /// "harmless", and [`crate::Group::self_update`] for the direct path that
    /// rotates a leaf without one.
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

/// The author of a proposal as the gate sees them: the **account** the
/// MLS-authenticated sender leaf resolves to in the **pre-merge** roster
/// (ADR-0050). A sender that cannot be resolved to a member, or whose leaf
/// resolves to no account, has no `Proposer` at all, and [`authorise`] refuses
/// every kind in that case. Every authority question is asked about the
/// account, never the leaf, so a person holds the same authority from each of
/// their devices.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Proposer {
    /// The account the sender leaf belongs to.
    pub account: [u8; 32],
}

/// Classify an OpenMLS [`Proposal`] into its Lakko [`ProposalKind`].
pub(crate) fn classify(proposal: &Proposal) -> ProposalKind {
    match proposal {
        Proposal::Add(add) => {
            let leaf = add.key_package().leaf_node();
            let signature_key = leaf.signature_key().as_slice().to_vec();
            let account_key = crate::device_binding::account_key_of_credential(
                leaf.credential().serialized_content(),
                &signature_key,
            )
            .map(|k| k.to_vec());
            ProposalKind::Add {
                signature_key,
                account_key,
            }
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

/// Resolve a leaf index to the **account** key the leaf belongs to
/// (ADR-0050) — `None` for a leaf that is not in `members` or whose credential
/// did not resolve.
pub(crate) fn account_key_at_leaf(members: &[MemberRecord], leaf: u32) -> Option<[u8; 32]> {
    members
        .iter()
        .find(|m| m.leaf_index == leaf)
        .and_then(|m| m.account_key.as_slice().try_into().ok())
}

/// The [`Proposer`] at `leaf`, or `None` when the leaf is not in `members` or
/// resolves to no account. **The** leaf-to-identity step every gate takes.
pub(crate) fn proposer_at(members: &[MemberRecord], leaf: u32) -> Option<Proposer> {
    account_key_at_leaf(members, leaf).map(|account| Proposer { account })
}

/// `true` when `leaf` currently belongs to the group owner's account.
///
/// A leaf that is not in `members` is **not** the owner — the same reading the
/// pre-ADR-0045 commit gate used, so an unknown leaf still falls through to the
/// owner-or-admin requirement below rather than being silently blessed.
fn leaf_is_owner(table: &RoleTable, members: &[MemberRecord], leaf: u32) -> bool {
    account_key_at_leaf(members, leaf).is_some_and(|k| table.is_owner(&k))
}

/// Number of leaves in `members` that resolve to `account`.
pub(crate) fn leaves_of_account(members: &[MemberRecord], account: &[u8]) -> usize {
    members
        .iter()
        .filter(|m| m.account_key.as_slice() == account)
        .count()
}

/// Upper bound on leaves one account may hold in a group (ADR-0050). Enforced
/// at the receipt gate and in the sweep filter, so the self-service `Add` arm
/// cannot be used to fill a group.
pub const MAX_LEAVES_PER_ACCOUNT: usize = 8;

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
/// | [`ProposalKind::Add`] of a leaf of the **proposer's own account** | anyone (ADR-0050: adding my own device) |
/// | [`ProposalKind::Add`] of a leaf of an account **already in the group** | anyone (ADR-0050: re-admitting a member's new device) |
/// | [`ProposalKind::Add`] of a leaf whose binding does not resolve | nobody |
/// | [`ProposalKind::Add`] of anyone else | owner or admin |
/// | [`ProposalKind::Remove`] of the owner account's **last** leaf | nobody — the owner is unremovable |
/// | [`ProposalKind::Remove`] of a leaf of the **proposer's own account** | anyone (leaving, or removing my own device) |
/// | [`ProposalKind::Remove`] of any other leaf | owner or admin |
/// | [`ProposalKind::Leave`] by the owner's **last** leaf | nobody — transfer first (see below) |
///
/// "Own account" is the ADR-0050 resolution of the leaf, so a person acting
/// from any of their devices holds the same authority. The owner's *last* leaf
/// is judged against the current roster here as a courtesy; the commit-level
/// rule [`owner_survives`] is what enforces it on every peer.
/// | [`ProposalKind::Leave`] by anyone else | anyone |
/// | [`ProposalKind::Update`] | nobody — see below |
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
/// # Why an `Update` is refused outright
///
/// An `Update` looks like the one proposal that could not possibly need a role:
/// RFC 9420 §12.1.2 says it replaces the **sender's own** leaf node and can name
/// no other. That reading is what this arm used to encode, and it is wrong twice
/// over — once on what an Update *does*, and once on what a *retained* Update
/// costs.
///
/// * **It rotates the key the role table is keyed on.** [`RoleTable`] names
///   32-byte Ed25519 **signature** keys, and openmls 0.8.1 does not bind an
///   Update's leaf node to the sender's *existing* one:
///   `UpdateProposalIn::validate` verifies the leaf against the signature key
///   carried **inside** it, `validate_update_proposals` still carries an
///   explicit `TODO: #133 … according to Sec. 13.2`, and
///   `validate_key_uniqueness` collects signature keys from `Add` proposals
///   only. So a member can swap its own leaf's signature key for one the table
///   has never heard of. If that member is the owner — or is being made owner
///   by a [`ProposalKind::RoleChange`] riding in the same sweep — the owner slot
///   then names a key no leaf holds, which is [`owner_survives`]' terminal
///   state and is **unrepairable**, because a role change is owner-only. And
///   `owner_survives` cannot see it coming: [`commit_effects`] has no field for
///   "this leaf's signature key changed", and giving it one would mean
///   reimplementing the leaf-node binding openmls has left open.
/// * **It is the one kind de-duplication cannot bound.** Every Update carries a
///   freshly generated leaf node, so N of them hash to N distinct
///   `ProposalRef`s. The dedup in [`crate::Group::gate_incoming_proposal`]
///   collapses a repeated self-removal precisely because a repeat is
///   byte-identical; it has nothing to collapse here. A **plain member** could
///   therefore fill every peer's pending set to its cap with stock proposals no
///   modified client is needed to emit, after which every membership operation
///   in the group — add, remove, role change, external-join approve — is refused
///   until somebody sweeps, and the flood resumes immediately afterwards.
///
/// **Nothing legitimate is lost.** `lakko-mls` emits no Update proposal on any
/// path and neither FFI surface exposes one; the way a Lakko member rotates its
/// leaf is [`crate::Group::self_update`], which drives the rotation through the
/// commit's `UpdatePath` (openmls `CommitBuilder::leaf_node_parameters`) and
/// composes **no** proposal at all — a committer may not carry its own Update
/// even in stock openmls (`filter_proposals` drops it; `validate_update_proposals`
/// refuses it as `CommitterIncludedOwnUpdate`). So this refusal removes the
/// ability to ask *somebody else* to commit your rotation, and nothing else.
///
/// It is deliberately **not** folded into [`ProposalKind::Unsupported`]: an
/// Update is supported and refused on purpose, and a refusal a developer cannot
/// attribute is its own defect.
///
/// # Errors
///
/// [`Error::Group`] when `proposer` is absent (unresolvable sender) or the
/// proposer's role does not authorise `kind`.
pub(crate) fn authorise(
    table: &RoleTable,
    members: &[MemberRecord],
    proposer: Option<Proposer>,
    kind: &ProposalKind,
) -> Result<()> {
    let Some(proposer) = proposer else {
        return Err(Error::refused(
            RefusalReason::NotAMember,
            "proposal refused: sender is not a resolvable group member",
        ));
    };
    let is_owner = table.is_owner(&proposer.account);
    let can_administer = table.can_administer(&proposer.account);

    match kind {
        ProposalKind::Add {
            account_key,
            signature_key: _,
        } => {
            let Some(added) = account_key else {
                return Err(Error::refused(
                    RefusalReason::DeviceBindingInvalid,
                    "add proposal refused: the added leaf's device binding does not verify",
                ));
            };
            let own_device = added.as_slice() == proposer.account;
            let account_already_present = leaves_of_account(members, added) > 0;
            if !can_administer && !own_device && !account_already_present {
                return Err(Error::refused(
                    RefusalReason::AddNotOwnerOrAdmin,
                    "add proposal refused: proposer is not owner or admin",
                ));
            }
        },
        ProposalKind::Remove { target_leaf } => {
            let target_account = account_key_at_leaf(members, *target_leaf);
            let own_account = target_account.is_some_and(|a| a == proposer.account);
            if leaf_is_owner(table, members, *target_leaf) {
                // The owner may retire one of its own devices as long as
                // another stays; the owner's last leaf is unremovable by
                // anyone, the owner included.
                let owner_keeps_a_leaf =
                    own_account && leaves_of_account(members, table.owner()) > 1;
                if !owner_keeps_a_leaf {
                    return Err(Error::refused(
                        RefusalReason::OwnerUnremovable,
                        "removal proposal refused: the group owner cannot be removed",
                    ));
                }
            }
            // Removing a leaf of your own account is leaving, or retiring one
            // of your devices; neither needs a role.
            if !own_account && !can_administer {
                return Err(Error::refused(
                    RefusalReason::RemoveNotOwnerOrAdmin,
                    "removal proposal refused: proposer is not owner or admin",
                ));
            }
        },
        ProposalKind::Leave => {
            if is_owner && leaves_of_account(members, table.owner()) <= 1 {
                return Err(Error::refused(
                    RefusalReason::OwnerCannotLeave,
                    "self-removal proposal refused: the group owner cannot leave",
                ));
            }
        },
        // Refused for everyone, including the owner. See the section above:
        // an Update can rotate the very signature key `table` is keyed on, and
        // it is the one kind whose ProposalRef is fresh every time, so it is
        // also the one a plain member can flood the pending set with.
        ProposalKind::Update => {
            return Err(Error::refused(
                RefusalReason::LeafUpdateRefused,
                "leaf-update proposal refused: an Update can rotate the signature key the role \
                 table is keyed on, and Lakko rotates a leaf with self_update instead",
            ));
        },
        ProposalKind::RoleChange => {
            if !is_owner {
                return Err(Error::refused(
                    RefusalReason::NotOwner,
                    "role-table change proposal refused: proposer is not the owner",
                ));
            }
        },
        ProposalKind::Unsupported => {
            return Err(Error::refused(
                RefusalReason::UnsupportedProposal,
                "proposal refused: unsupported proposal type",
            ));
        },
    }
    Ok(())
}

/// `true` when this proposal is the group **owner** exercising its authority
/// over the group's membership structure — a `Remove` or a `RoleChange` it
/// authored itself.
///
/// # The escalation this exists to close
///
/// The pending set is bounded ([`crate::group::MAX_PENDING_PROPOSALS`]), and the
/// bound is enforced without regard to who is asking: at the cap
/// `Group::authorise_own_proposal` refuses **every** proposal, the owner's
/// included, and `Group::refuse_if_proposals_pending` refuses every direct
/// commit. Nothing anywhere discards a pending proposal without committing it.
/// Composed, a member who can fill the set takes away the owner's ability to
/// remove them, to demote them, and to discard what they filed.
///
/// [`crate::group::MAX_PENDING_PROPOSALS`] used to argue that this was harmless
/// for the one ask that can still fill the set — `Add`, whose fresh KeyPackage
/// defeats de-duplication — on the ground that it needs owner-or-admin, "a role
/// that can already remove every member, so filling this set is strictly weaker
/// than what it already holds". **That reasoning is wrong, and the shape of the
/// error is worth keeping.** It compared the two capabilities and missed that
/// they compose: mass removal is loud, attributable and repairable — the owner
/// re-adds — whereas filling the pending set removes the *response*. The only
/// exit was to sweep, which enacts the flood and leaves the floodgate holder
/// free to re-flood at once. A capability that takes away the answer to itself
/// is an escalation, not a weaker sibling. `Remove` reaches the same place by a
/// second route: a repeat naming one target de-duplicates, but one member may
/// name every *other* member, so in a large group an admin can fill the set with
/// removals nothing collapses.
///
/// # The rule
///
/// A response is admitted past the ordinary cap, into a reserve that only the
/// owner can spend ([`crate::group::MAX_RESERVED_OWNER_RESPONSES`]). Three
/// properties make that safe:
///
/// * **It grants nothing.** The owner may already remove a member and rewrite
///   the role table; ADR-0022's gate is untouched and still runs afterwards, so
///   this only decides *when the set is too full to accept a request*, never
///   *who may make one*. A proposal that reaches the reserve and is then refused
///   by [`authorise`] is still refused.
/// * **The reserve cannot be spent by anyone else.** Authorship is the
///   MLS-authenticated sender leaf resolved against the pre-merge roster, so an
///   attacker cannot present itself as the owner; and `Add` is deliberately not
///   a response, so the owner cannot inflate the group's own bound through it.
/// * **The bound stays finite.** Capacity moves from one constant to two, and
///   the hard ceiling is their sum.
///
/// Both cap sites — the local courtesy check and the peers' receipt gate — must
/// ask this identical question, or the owner would publish a response its peers
/// refuse to retain and then sweep a commit they cannot resolve. That is why the
/// predicate is written down here, once, beside the rule it qualifies.
pub(crate) fn is_owner_response(
    table: &RoleTable,
    proposer: Option<Proposer>,
    kind: &ProposalKind,
) -> bool {
    let Some(proposer) = proposer else {
        return false;
    };
    table.is_owner(&proposer.account)
        && matches!(kind, ProposalKind::Remove { .. } | ProposalKind::RoleChange)
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
    /// The accounts those added leaves resolve to (ADR-0050), in the same
    /// order; `None` for a leaf whose binding does not resolve.
    added_accounts: Vec<Option<Vec<u8>>>,
    /// The group-kind facts (ADR-0060) each `GroupContextExtensions` proposal
    /// the commit carries would install, one entry per such proposal. Its
    /// length is also the number of GroupContext rewrites the commit carries.
    installed_kinds: Vec<KindState>,
}

/// Summarise what `proposals` would do if committed together.
pub(crate) fn commit_effects<'a>(
    proposals: impl Iterator<Item = &'a QueuedProposal>,
) -> CommitEffects {
    let mut effects = CommitEffects::default();
    for qp in proposals {
        match classify(qp.proposal()) {
            ProposalKind::Add {
                signature_key,
                account_key,
            } => {
                effects.added_keys.push(signature_key);
                effects.added_accounts.push(account_key);
            },
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
                // the fields these rules need are read from the raw proposal.
                match role_table_of(qp.proposal()) {
                    Some(table) => effects.proposed_owners.push(table.owner().to_vec()),
                    None => effects.role_table_would_vanish = true,
                }
                effects
                    .installed_kinds
                    .push(installed_kind_of(qp.proposal()).unwrap_or_default());
            },
            // Neither can appear in a set this summary is trusted over:
            // `authorise` refuses both at all three of its call sites, so an
            // Update never reaches the pending store, never survives the sweep
            // filter, and never rides a commit a peer merges. That matters,
            // because there is no honest arm to write here — an Update changes
            // a leaf's signature key, which is exactly the field
            // `owner_survives` matches on, and `CommitEffects` has nowhere to
            // record it. Keeping one out of every commit is the fix; see
            // `authorise`.
            ProposalKind::Update | ProposalKind::Unsupported => {},
        }
    }
    effects
}

impl CommitEffects {
    /// The effects of a **direct** membership commit this device is about to
    /// build (`add_members` / `remove_member`), for the local courtesy check of
    /// [`one_to_one_holds`] before any MLS state is touched.
    pub(crate) fn direct(removed_leaves: Vec<u32>, added_accounts: Vec<Option<Vec<u8>>>) -> Self {
        Self {
            removed_leaves,
            added_accounts,
            ..Self::default()
        }
    }

    /// The leaves this set of proposals removes, including a `SelfRemove`'s own
    /// sender leaf.
    ///
    /// The sweep filter needs it to decide whether an `Add` collides with a
    /// signature key the group already holds: RFC 9420 requires the added keys
    /// to be unique against the roster the commit *produces*, so a key belonging
    /// to a member the same commit removes does **not** collide (openmls
    /// `validate_key_uniqueness` seeds its sets from `full_leaf_members()`
    /// filtered by the commit's remove proposals). A rule that ignored the
    /// removals would drop a legitimate remove-and-re-add.
    pub(crate) fn removed_leaves(&self) -> &[u32] {
        &self.removed_leaves
    }
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

/// The group-kind facts a `GroupContextExtensions` proposal would install, or
/// `None` for any other proposal.
pub(crate) fn installed_kind_of(proposal: &Proposal) -> Option<KindState> {
    let Proposal::GroupContextExtensions(gce) = proposal else {
        return None;
    };
    Some(KindState::of(gce.extensions()))
}

/// `true` when `proposal` leaves the group's kind as `before` records it — any
/// proposal that is not a GroupContext rewrite trivially does (ADR-0060
/// section 4). The per-proposal form of [`group_kind_unchanged`], shared by the
/// receipt gate and the sweep filter so the two cannot disagree.
pub(crate) fn proposal_keeps_group_kind(proposal: &Proposal, before: &KindState) -> bool {
    installed_kind_of(proposal).map_or(true, |after| before.preserved_by(&after))
}

/// **No commit changes a conversation's kind** (ADR-0060 section 4, C-70).
///
/// Every `GroupContextExtensions` proposal the commit carries must install an
/// `0xFF03` body byte-identical to the pre-merge one — present in both or
/// absent in both — and must not drop `0xFF03` from `RequiredCapabilities`.
/// Judged against `before`, the **pre-merge** GroupContext, for the same reason
/// the role table is (ADR-0022 Finding 2): a commit is judged against what it
/// inherits, never what it writes.
///
/// # Errors
///
/// A [`RefusalReason::GroupKindChanged`] refusal when any rewrite changes the
/// kind.
pub(crate) fn group_kind_unchanged(effects: &CommitEffects, before: &KindState) -> Result<()> {
    if effects
        .installed_kinds
        .iter()
        .all(|after| before.preserved_by(after))
    {
        return Ok(());
    }
    Err(Error::refused(
        RefusalReason::GroupKindChanged,
        "commit refused: it would change or remove this conversation's kind",
    ))
}

/// **A one-to-one keeps its two people** (ADR-0060 section 5, C-70) — the
/// commit-wide rule, in the style of [`owner_survives`].
///
/// For a one-to-one, the accounts in the roster the commit produces (pre-merge
/// members, minus every removed leaf, plus every added leaf's account,
/// ADR-0050) must contain every pre-merge account and number at most two; and
/// the commit may carry no GroupContext rewrite at all, because a one-to-one
/// has no roles and its GroupContext is frozen from creation. With two people
/// present that means the set of people never changes; with the founder alone
/// it admits exactly one other person, with any number of that person's device
/// leaves — the founding commit. A person's own devices may come and go.
///
/// For a group, or a group with no kind at all, it always passes.
///
/// It has to see the whole commit: two `Remove`s of one account's two leaves
/// are each harmless alone and together remove a person.
///
/// An added leaf whose account does not resolve counts as a stranger and is
/// refused (fail-closed, invariant #4); every other gate refuses such a leaf
/// anyway.
///
/// # Errors
///
/// A [`RefusalReason::OneToOneHasNoRoles`] refusal for a GroupContext rewrite,
/// a [`RefusalReason::OneToOneMembershipFixed`] refusal for a change of people.
pub(crate) fn one_to_one_holds(
    effects: &CommitEffects,
    members: &[MemberRecord],
    kind: Option<GroupKind>,
) -> Result<()> {
    if kind != Some(GroupKind::OneToOne) {
        return Ok(());
    }
    if !effects.installed_kinds.is_empty() {
        return Err(Error::refused(
            RefusalReason::OneToOneHasNoRoles,
            "commit refused: a one-to-one has no roles and its GroupContext never changes",
        ));
    }
    let fixed = || {
        Err(Error::refused(
            RefusalReason::OneToOneMembershipFixed,
            "commit refused: a one-to-one always has the same two people",
        ))
    };
    let mut after: Vec<&[u8]> = Vec::new();
    for m in members {
        if !effects.removed_leaves.contains(&m.leaf_index)
            && !after.contains(&m.account_key.as_slice())
        {
            after.push(m.account_key.as_slice());
        }
    }
    for added in &effects.added_accounts {
        let Some(account) = added.as_deref() else {
            return fixed();
        };
        if !after.contains(&account) {
            after.push(account);
        }
    }
    let everyone_stays = members
        .iter()
        .all(|m| after.contains(&m.account_key.as_slice()));
    if !everyone_stays || after.len() > 2 {
        return fixed();
    }
    Ok(())
}

/// The early, per-proposal answer to the one-to-one rule (ADR-0060 section 5),
/// run where [`authorise`] runs for a proposal — at the proposer and at the
/// receipt gate — so a request that could never be committed fails at once and
/// never sits in anyone's pending set.
///
/// In a one-to-one:
///
/// | Proposal | Answer |
/// |---|---|
/// | `Add` of a leaf whose account is already in the group | allowed (own device, or re-admission) |
/// | `Add` of a leaf of any other account | refused |
/// | `Remove` of a leaf whose account keeps another leaf | allowed (retiring a device) |
/// | `Remove` of an account's last leaf, or of an unknown leaf | refused |
/// | `Leave` from an account's last leaf | refused |
/// | `Leave` from a leaf whose account keeps another | allowed |
/// | `RoleChange` | refused |
///
/// Everything it allows still has to pass [`authorise`]. An `Add` whose binding
/// does not resolve and an unresolvable proposer are left to `authorise`, which
/// refuses both with their own reasons.
///
/// A per-proposal rule cannot see two proposals together, so this is the early
/// answer, never the authority: [`one_to_one_holds`] is, at the sweep and the
/// peer commit gate.
///
/// # Errors
///
/// [`RefusalReason::OneToOneMembershipFixed`] or
/// [`RefusalReason::OneToOneHasNoRoles`] per the table above.
pub(crate) fn one_to_one_allows(
    kind: Option<GroupKind>,
    members: &[MemberRecord],
    proposer: Option<Proposer>,
    proposal: &ProposalKind,
) -> Result<()> {
    if kind != Some(GroupKind::OneToOne) {
        return Ok(());
    }
    let fixed = |detail: &str| {
        Err(Error::refused(
            RefusalReason::OneToOneMembershipFixed,
            detail,
        ))
    };
    match proposal {
        ProposalKind::Add {
            account_key: Some(account),
            ..
        } => {
            if leaves_of_account(members, account) == 0 {
                return fixed("add proposal refused: a one-to-one always has the same two people");
            }
        },
        ProposalKind::Remove { target_leaf } => {
            let keeps_another = account_key_at_leaf(members, *target_leaf)
                .is_some_and(|account| leaves_of_account(members, &account) > 1);
            if !keeps_another {
                return fixed(
                    "removal proposal refused: a one-to-one always has the same two people",
                );
            }
        },
        ProposalKind::Leave => {
            if let Some(proposer) = proposer {
                if leaves_of_account(members, &proposer.account) <= 1 {
                    return fixed(
                        "self-removal proposal refused: a person cannot leave a one-to-one",
                    );
                }
            }
        },
        ProposalKind::RoleChange => {
            return Err(Error::refused(
                RefusalReason::OneToOneHasNoRoles,
                "role-table change proposal refused: a one-to-one has no roles",
            ));
        },
        ProposalKind::Add {
            account_key: None, ..
        }
        | ProposalKind::Update
        | ProposalKind::Unsupported => {},
    }
    Ok(())
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
pub(crate) fn owner_survives(
    effects: &CommitEffects,
    members: &[MemberRecord],
    current_owner: &[u8],
) -> Result<()> {
    if effects.role_table_would_vanish {
        return Err(Error::refused(
            RefusalReason::RoleTableWouldVanish,
            "commit refused: it would remove this group's role table",
        ));
    }
    // ADR-0050: an owner is an account, and it survives when at least one of
    // its leaves is in the roster the commit produces — a leaf that stays, or
    // one the commit adds.
    let survives = |key: &[u8]| {
        let stays = members.iter().any(|m| {
            m.account_key.as_slice() == key && !effects.removed_leaves.contains(&m.leaf_index)
        });
        stays
            || effects
                .added_accounts
                .iter()
                .any(|a| a.as_deref() == Some(key))
    };
    if effects.proposed_owners.is_empty() {
        // No role change: the current owner must still hold a leaf afterwards,
        // or the group could never be administered again.
        if !survives(current_owner) {
            return Err(Error::refused(
                RefusalReason::OwnerUnremovable,
                "commit refused: it would remove the group owner's last device",
            ));
        }
        return Ok(());
    }
    for owner in &effects.proposed_owners {
        if !survives(owner) {
            return Err(Error::refused(
                RefusalReason::OwnerNotAMember,
                "role-table change refused: the named owner would not be a member of the group",
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
                account_key: pk(1).to_vec(),
            },
            MemberRecord {
                leaf_index: 1,
                signature_public_key: pk(2).to_vec(),
                account_key: pk(2).to_vec(),
            },
            MemberRecord {
                leaf_index: 2,
                signature_public_key: pk(3).to_vec(),
                account_key: pk(3).to_vec(),
            },
        ];
        (table, members)
    }

    fn proposer(_leaf: u32, key: &[u8]) -> Proposer {
        Proposer {
            account: key.try_into().expect("32-byte test key"),
        }
    }

    fn add_kind() -> ProposalKind {
        ProposalKind::Add {
            signature_key: pk(9).to_vec(),
            account_key: Some(pk(9).to_vec()),
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

    /// An Update is refused for **every** role, owner included. It is not a
    /// permission question: an Update can rotate the signature key the role
    /// table is keyed on, and being the owner is no reason to be allowed to walk
    /// out of the table.
    #[test]
    fn update_is_refused_for_every_role() {
        let (t, m) = fixture();
        for (leaf, key) in [(0u32, pk(1)), (1, pk(2)), (2, pk(3))] {
            let err = authorise(&t, &m, Some(proposer(leaf, &key)), &ProposalKind::Update)
                .expect_err("an Update must be refused at every role");
            let text = format!("{err}");
            assert!(
                text.contains("leaf-update"),
                "the refusal must name itself, got {text:?}",
            );
            assert!(
                !text.contains("unsupported"),
                "an Update is refused on purpose, not for being unrecognised: {text:?}",
            );
        }
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

    /// The reserve predicate, in full. Only the owner, and only for the two
    /// kinds that ARE the response to somebody filling the pending set.
    ///
    /// Every `false` here is load-bearing. An admin is the attacker in the
    /// escalation this closes, so a reserve it could spend would be no reserve;
    /// `Add` is what the flood is made of, so admitting the owner's own past the
    /// cap would let the owner inflate the group's bound; and an unresolvable
    /// sender is refused for the same fail-closed reason [`authorise`] refuses
    /// it.
    #[test]
    fn only_the_owners_removal_and_role_change_claim_the_reserve() {
        let (t, _m) = fixture();
        let owner = pk(1);
        let admin = pk(2);
        let member = pk(3);
        let remove = ProposalKind::Remove { target_leaf: 2 };

        assert!(is_owner_response(&t, Some(proposer(0, &owner)), &remove));
        assert!(is_owner_response(
            &t,
            Some(proposer(0, &owner)),
            &ProposalKind::RoleChange
        ));

        for (label, kind) in [
            ("add", add_kind()),
            ("leave", ProposalKind::Leave),
            ("update", ProposalKind::Update),
            ("unsupported", ProposalKind::Unsupported),
        ] {
            assert!(
                !is_owner_response(&t, Some(proposer(0, &owner)), &kind),
                "the owner's {label} is not a response",
            );
        }
        for (label, key, leaf) in [("admin", admin, 1u32), ("member", member, 2)] {
            for kind in [remove.clone(), ProposalKind::RoleChange] {
                assert!(
                    !is_owner_response(&t, Some(proposer(leaf, &key)), &kind),
                    "an {label} claims no reserve for {kind:?}",
                );
            }
        }
        assert!(
            !is_owner_response(&t, None, &remove),
            "an unresolvable sender claims no reserve",
        );
    }

    /// Every arm of the one rule reports its **own** reason.
    ///
    /// This is the assertion the six §6 refusal strings rest on, and it is
    /// written against `authorise` rather than against a `Group` method because
    /// two of the arms — the owner's `SelfRemove` and an `Update` — are reachable
    /// only from the receipt and commit gates. No propose API emits either, so
    /// nothing else in the suite can reach them.
    #[test]
    fn every_authorise_arm_reports_its_own_reason() {
        let (t, m) = fixture();
        let owner = pk(1);
        let admin = pk(2);
        let member = pk(3);

        let cases: Vec<(&str, Option<Proposer>, ProposalKind, RefusalReason)> = vec![
            (
                "unresolvable sender",
                None,
                add_kind(),
                RefusalReason::NotAMember,
            ),
            (
                "a member's add",
                Some(proposer(2, &member)),
                add_kind(),
                RefusalReason::AddNotOwnerOrAdmin,
            ),
            (
                "a member removing another",
                Some(proposer(2, &member)),
                ProposalKind::Remove { target_leaf: 1 },
                RefusalReason::RemoveNotOwnerOrAdmin,
            ),
            (
                "an admin removing the owner",
                Some(proposer(1, &admin)),
                ProposalKind::Remove { target_leaf: 0 },
                RefusalReason::OwnerUnremovable,
            ),
            (
                "the owner leaving",
                Some(proposer(0, &owner)),
                ProposalKind::Leave,
                RefusalReason::OwnerCannotLeave,
            ),
            (
                "an admin's role change",
                Some(proposer(1, &admin)),
                ProposalKind::RoleChange,
                RefusalReason::NotOwner,
            ),
            (
                "an owner's leaf update",
                Some(proposer(0, &owner)),
                ProposalKind::Update,
                RefusalReason::LeafUpdateRefused,
            ),
            (
                "an owner's unsupported proposal",
                Some(proposer(0, &owner)),
                ProposalKind::Unsupported,
                RefusalReason::UnsupportedProposal,
            ),
        ];

        for (label, who, kind, expected) in cases {
            let err = authorise(&t, &m, who, &kind).expect_err(label);
            assert_eq!(
                err.refusal_reason(),
                Some(expected),
                "{label} reported the wrong reason: {err:?}",
            );
        }
    }

    /// The `owner_survives` commit rule has two refusals, and they must not
    /// collapse: one says the table would name an owner outside the group, the
    /// other that the commit would delete the table altogether.
    #[test]
    fn owner_survives_reports_its_two_reasons_apart() {
        let (_t, m) = fixture();

        let vanishing = CommitEffects {
            role_table_would_vanish: true,
            ..CommitEffects::default()
        };
        assert_eq!(
            owner_survives(&vanishing, &m, &pk(1))
                .expect_err("an erased role table must be refused")
                .refusal_reason(),
            Some(RefusalReason::RoleTableWouldVanish),
        );

        let stranger = CommitEffects {
            proposed_owners: vec![pk(9).to_vec()],
            ..CommitEffects::default()
        };
        assert_eq!(
            owner_survives(&stranger, &m, &pk(1))
                .expect_err("an owner outside the group must be refused")
                .refusal_reason(),
            Some(RefusalReason::OwnerNotAMember),
        );

        // And a commit that removes the very leaf it names owner.
        let self_removing = CommitEffects {
            proposed_owners: vec![pk(3).to_vec()],
            removed_leaves: vec![2],
            ..CommitEffects::default()
        };
        assert_eq!(
            owner_survives(&self_removing, &m, &pk(1))
                .expect_err("an owner the same commit removes must be refused")
                .refusal_reason(),
            Some(RefusalReason::OwnerNotAMember),
        );
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

    // ---- ADR-0060: the one-to-one rule ------------------------------------

    fn rec(leaf: u32, account: u8) -> MemberRecord {
        MemberRecord {
            leaf_index: leaf,
            signature_public_key: pk(100 + u8::try_from(leaf).expect("small")).to_vec(),
            account_key: pk(account).to_vec(),
        }
    }

    /// Alice (account 1) on leaves 0 and 1, Bob (account 2) on leaves 2 and 3.
    fn two_people_four_leaves() -> Vec<MemberRecord> {
        vec![rec(0, 1), rec(1, 1), rec(2, 2), rec(3, 2)]
    }

    const ONE: Option<GroupKind> = Some(GroupKind::OneToOne);

    fn reason_of(r: Result<()>) -> Option<RefusalReason> {
        r.err().and_then(|e| e.refusal_reason())
    }

    #[test]
    fn a_group_or_a_kindless_group_is_never_held_to_the_one_to_one_rule() {
        let members = two_people_four_leaves();
        let effects = CommitEffects::direct(vec![2, 3], vec![Some(pk(9).to_vec())]);
        assert!(one_to_one_holds(&effects, &members, Some(GroupKind::Group)).is_ok());
        assert!(one_to_one_holds(&effects, &members, None).is_ok());
    }

    #[test]
    fn two_removals_of_one_persons_leaves_break_the_rule_though_each_alone_does_not() {
        let members = two_people_four_leaves();
        for single in [2u32, 3] {
            assert!(
                one_to_one_holds(&CommitEffects::direct(vec![single], vec![]), &members, ONE)
                    .is_ok(),
                "retiring one of bob's two devices is fine"
            );
        }
        assert_eq!(
            reason_of(one_to_one_holds(
                &CommitEffects::direct(vec![2, 3], vec![]),
                &members,
                ONE
            )),
            Some(RefusalReason::OneToOneMembershipFixed),
        );
    }

    #[test]
    fn the_founding_commit_admits_exactly_one_other_person() {
        let founder = vec![rec(0, 1)];
        let one_other = CommitEffects::direct(
            vec![],
            vec![
                Some(pk(2).to_vec()),
                Some(pk(2).to_vec()),
                Some(pk(1).to_vec()),
            ],
        );
        assert!(one_to_one_holds(&one_other, &founder, ONE).is_ok());
        let two_others =
            CommitEffects::direct(vec![], vec![Some(pk(2).to_vec()), Some(pk(3).to_vec())]);
        assert_eq!(
            reason_of(one_to_one_holds(&two_others, &founder, ONE)),
            Some(RefusalReason::OneToOneMembershipFixed),
        );
        let unresolved = CommitEffects::direct(vec![], vec![None]);
        assert_eq!(
            reason_of(one_to_one_holds(&unresolved, &founder, ONE)),
            Some(RefusalReason::OneToOneMembershipFixed),
            "an added leaf with no account is a stranger, fail-closed",
        );
    }

    #[test]
    fn the_per_proposal_answer_matches_the_adr_table() {
        let members = two_people_four_leaves();
        let alice = Some(Proposer { account: pk(1) });
        let bob_alone = vec![rec(0, 1), rec(1, 1), rec(2, 2)];
        let add = |a: u8| ProposalKind::Add {
            signature_key: pk(50).to_vec(),
            account_key: Some(pk(a).to_vec()),
        };
        assert!(one_to_one_allows(ONE, &members, alice, &add(2)).is_ok());
        assert!(one_to_one_allows(ONE, &members, alice, &add(1)).is_ok());
        assert_eq!(
            reason_of(one_to_one_allows(ONE, &members, alice, &add(3))),
            Some(RefusalReason::OneToOneMembershipFixed),
        );
        let remove = |l: u32| ProposalKind::Remove { target_leaf: l };
        assert!(one_to_one_allows(ONE, &members, alice, &remove(3)).is_ok());
        assert_eq!(
            reason_of(one_to_one_allows(ONE, &bob_alone, alice, &remove(2))),
            Some(RefusalReason::OneToOneMembershipFixed),
        );
        assert_eq!(
            reason_of(one_to_one_allows(ONE, &members, alice, &remove(99))),
            Some(RefusalReason::OneToOneMembershipFixed),
            "an unknown leaf is refused rather than reasoned about",
        );
        let bob = Some(Proposer { account: pk(2) });
        assert!(one_to_one_allows(ONE, &members, bob, &ProposalKind::Leave).is_ok());
        assert_eq!(
            reason_of(one_to_one_allows(
                ONE,
                &bob_alone,
                bob,
                &ProposalKind::Leave
            )),
            Some(RefusalReason::OneToOneMembershipFixed),
        );
        assert_eq!(
            reason_of(one_to_one_allows(
                ONE,
                &members,
                alice,
                &ProposalKind::RoleChange
            )),
            Some(RefusalReason::OneToOneHasNoRoles),
        );
        assert!(one_to_one_allows(Some(GroupKind::Group), &members, alice, &add(3)).is_ok());
    }

    proptest::proptest! {
        /// Whatever a commit adds and removes, the rule passes in a one-to-one
        /// exactly when every pre-merge account survives and at most two
        /// accounts remain — checked against an independent restatement.
        #[test]
        fn the_one_to_one_rule_is_exactly_subset_and_at_most_two(
            removed_mask in 0u8..16,
            added in proptest::collection::vec(1u8..5, 0..4),
        ) {
            let members = two_people_four_leaves();
            let removed: Vec<u32> = (0..4u32).filter(|l| removed_mask & (1 << l) != 0).collect();
            let effects = CommitEffects::direct(
                removed.clone(),
                added.iter().map(|a| Some(pk(*a).to_vec())).collect(),
            );
            let mut after: Vec<u8> = members
                .iter()
                .filter(|m| !removed.contains(&m.leaf_index))
                .map(|m| m.account_key[0])
                .chain(added.iter().copied())
                .collect();
            after.sort_unstable();
            after.dedup();
            let expect = after.contains(&1) && after.contains(&2) && after.len() <= 2;
            proptest::prop_assert_eq!(
                one_to_one_holds(&effects, &members, ONE).is_ok(),
                expect
            );
        }
    }
}
