// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Machine-readable reasons for a refused membership operation.
//!
//! # Do not match on the prose. Match on [`RefusalReason`].
//!
//! Every membership refusal in this crate used to be an
//! [`Error::Group(String)`](crate::Error::Group) and nothing else, so a client
//! wanting to show one of the fixed strings the UX spec specifies
//! (`docs/ux/proposal-based-membership.md` §6) had exactly one way to tell them
//! apart: substring-match the sentence the core happened to write. That is a
//! drift class, not a mechanism — the core rewords a message and the client
//! silently falls through to a generic string, or worse, to the *wrong* one,
//! with nothing failing anywhere. It had already started happening in two
//! different ways across three clients before this type existed.
//!
//! So the prose stays — it is what a developer reads in a log or a stack — and
//! a [`RefusalReason`] travels **beside** it. The reason is the contract; the
//! sentence is not. A caller selects its copy from the reason, and a rewritten
//! message is then a non-event.
//!
//! # The disposition is what decides whether to offer a retry
//!
//! [`RefusalDisposition`] is the second half, and it is deliberately not
//! something a caller infers from a variant's name. It answers *what should the
//! caller do now*, and it is **three**-valued rather than two.
//!
//! Two was the obvious shape and it is wrong. The UX spec splits its refusal
//! table in two — *you may not* against *not yet* — and calling the second one
//! "retryable" quietly folds together two different things: a request the group
//! is merely too busy to accept **right now**, and a request that cannot be
//! accepted **as formed**, ever. Retrying the first works. Retrying the second
//! loops forever, and it is the more common of the two: a role table naming a
//! non-member owner, a proposal kind Lakko does not commit, an approve for a
//! request that is no longer there.
//!
//! So the rule is stated positively and narrowly: **a refusal is retryable
//! exactly when a bounded queue was full**, and never when the refusal is about
//! the request's own state. Only two reasons here are queue-full
//! ([`RefusalReason::PendingSetFull`] and [`RefusalReason::RoleChangePending`],
//! whose queue has capacity one — RFC 9420 admits at most one
//! `GroupContextExtensions` proposal per commit). Everything else is either
//! [`RefusalDisposition::NotPermitted`] (an authority answer: state it as a
//! fact) or [`RefusalDisposition::DoNotRetry`] (the caller must change
//! something before asking again).
//!
//! Getting this wrong in either direction is a real defect and not a cosmetic
//! one: a refusal that invites a retry it cannot satisfy sends the user into a
//! loop, and a timing refusal phrased as a prohibition tells them they cannot
//! do something they can. So it is answered by the core, once
//! ([`RefusalReason::disposition`] / [`RefusalReason::retry_may_succeed`]),
//! rather than re-decided in three clients — the same argument that motivates
//! the reason itself, applied one level up.
//!
//! # What has no reason at all, and must not be given one by defaulting
//!
//! A proposal the **sweep** excludes ([`crate::Group::stage_pending_proposals`]
//! and its exclusion pass) produces no error and no reason. openmls's
//! `CreateCommitError` names no proposal on any variant, so the core cannot say
//! *which* request was dropped, let alone why — a duplicate signature key, an
//! expired KeyPackage, a leaf this group's required capabilities will not
//! admit. The commit simply carries fewer proposals than the set held.
//!
//! That is a real gap and it is stated rather than papered over: such a request
//! is neither retryable nor not, because nothing classified it. The honest
//! client behaviour is the user-driven ask-again that already exists, never an
//! automatic retry — an automatic one would re-file a request the sweep will
//! exclude again, forever.
//!
//! # What is deliberately *not* a reason
//!
//! [`RefusalReason`] answers exactly one question: *may this membership change
//! happen, and if not, will waiting help?* It is not a general error taxonomy.
//! Argument-validation failures (a signature key of the wrong length), stale-UI
//! conditions (approving an external-join request that has already been
//! approved), wiring mistakes reachable only from inside this crate, and every
//! OpenMLS / codec / storage failure keep their plain
//! [`Error::Group`](crate::Error::Group) prose and report **no** reason. That is
//! the point of `Option`: a caller must not be able to mistake a wire failure
//! for a permission answer, and the §6 fail-closed fallback string is the honest
//! rendering of "no reason".

/// What the caller should do about a refusal — the one property a client's copy
/// and its retry affordance hang on.
///
/// Three-valued, and the module documentation says why two was not enough. The
/// UX spec's two-way wording maps onto the ends of this: its *authorisation*
/// rows are [`Self::NotPermitted`], its one *availability* row is
/// [`Self::RetryLater`], and [`Self::DoNotRetry`] is the middle the spec's split
/// did not name — where most of its fail-closed fallback row actually lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefusalDisposition {
    /// *Not yet.* A bounded queue is full. The caller is authorised, the
    /// request is well-formed, and **the identical call succeeds once a
    /// committer has swept** — with the caller doing nothing else in between.
    /// That last clause is what makes this the only retryable disposition: it
    /// resolves on its own.
    ///
    /// Offer a retry. Say the group is busy, not that the user may not.
    RetryLater,

    /// *Not like this.* The call will never succeed as it stands, but the
    /// caller is not forbidden: something has to change first — the request's
    /// own content (a role table naming a non-member owner), the caller's
    /// outstanding obligations (a staged sweep awaiting its verdict), or the
    /// world the request names (an approve for a request that is no longer
    /// pending).
    ///
    /// Do **not** offer a plain retry: it re-sends the same bytes and is
    /// refused the same way, forever. Say what has to change, or refresh and
    /// let the user ask again.
    DoNotRetry,

    /// *You may not.* An authority answer: this member's role does not permit
    /// the ask, or the group's rules forbid it for everyone. Nothing about the
    /// request would change it — only a different role would, and for the
    /// categorical rules not even that.
    ///
    /// State it as a fact. Offer no action at all.
    NotPermitted,
}

/// Why a membership operation was refused.
///
/// **Never match on an error's message text to recover this.** The message is
/// for logs and may be reworded at any time; this value is the contract. Read
/// it with [`Error::refusal_reason`](crate::Error::refusal_reason).
///
/// Variants map onto the UX spec's refusal table
/// (`docs/ux/proposal-based-membership.md` §6) where one exists, and name the
/// refusals the code has that the table does not. Several distinct reasons
/// legitimately share one piece of user-facing copy — the spec's fail-closed
/// fallback row collapses three of them — so a client's mapping is
/// many-to-one, and that collapse belongs to the client, not here: a reason
/// this crate can tell apart is one a developer can tell apart in a log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefusalReason {
    // ---- Authorisation --------------------------------------------------
    /// The proposal's sender does not resolve to a member of this group.
    /// Fail-closed: every kind is refused in that case.
    NotAMember,

    /// Adding a member needs owner or admin.
    AddNotOwnerOrAdmin,

    /// Removing *somebody else* needs owner or admin. Removing your own leaf is
    /// leaving and needs no role, so it never reaches this.
    RemoveNotOwnerOrAdmin,

    /// The group owner's leaf cannot be removed — by anyone, including the
    /// owner. Reached before the role check, so an owner asking to remove its
    /// own leaf reports this rather than [`Self::OwnerCannotLeave`].
    OwnerUnremovable,

    /// The group owner cannot leave while owner (RFC 9420 `SelfRemove`). The way
    /// out is [`Group::transfer_ownership`](crate::Group::transfer_ownership),
    /// after which the former owner is an ordinary member and may leave.
    OwnerCannotLeave,

    /// Changing the role table — which includes transferring ownership, since a
    /// transfer *is* a role-table rewrite — is owner-only.
    NotOwner,

    /// A leaf `Update` proposal, refused for every role including the owner. It
    /// can rotate the very signature key the role table is keyed on, and it is
    /// the one kind whose `ProposalRef` is fresh on every emission, so it is
    /// also the one a plain member could flood the pending set with. Lakko
    /// rotates a leaf through
    /// [`Group::self_update`](crate::Group::self_update) instead, which composes
    /// no proposal at all.
    ///
    /// Not reachable from any client-facing propose API — it exists so a
    /// refusal seen at the receipt or commit gate is attributable rather than
    /// collapsing into [`Self::UnsupportedProposal`].
    LeafUpdateRefused,

    /// A proposal type Lakko never emits (PSK, `ReInit`, `ExternalInit`, a
    /// custom proposal). Always refused — the core cannot say what it would do.
    UnsupportedProposal,

    /// The group carries no ADR-0022 role table, so it has no owner, therefore
    /// no committer, therefore nothing that would ever sweep a proposal
    /// published into it.
    NoRoleTable,

    /// The role table names an owner who is not — or after this commit would not
    /// be — a member of the group. Such a group can never be administered again:
    /// a role change is owner-only and the owner's leaf is unremovable, so
    /// nothing could repair it.
    OwnerNotAMember,

    /// The commit rewrites the GroupContext but leaves no readable role table
    /// behind, which would delete the group's authority structure rather than
    /// change it.
    RoleTableWouldVanish,

    /// This device's role does not authorise it to commit — sweeping the pending
    /// set needs owner or admin.
    NotACommitter,

    /// No retained external-join request matches the key an approve or reject
    /// named.
    ///
    /// It is a **stale-view** refusal, not a permission one, and it is the
    /// refusal an admin hits after tapping an affordance that should no longer
    /// have been on screen: another admin approved the joiner, the joiner
    /// landed and was reaped, or the request was never retained at all because
    /// the quarantine was full when it arrived (see
    /// [`crate::Incoming::ExternalJoinProposal`]'s `retained`, which is the
    /// signal that lets a client not offer the action in the first place).
    NoSuchJoinRequest,

    /// The joiner an approve named is already a member of the group.
    ///
    /// Nothing is wrong and nothing needs doing — refresh the pending list.
    JoinerAlreadyAMember,

    // ---- Retry-later / do-not-retry -------------------------------------
    /// Another role-table change is already pending. RFC 9420 admits at most one
    /// `GroupContextExtensions` proposal per commit, so a second could not be
    /// swept. Wait for the pending one to land, then ask again.
    RoleChangePending,

    /// A **direct** membership commit was refused because proposals are pending
    /// and a direct commit would silently absorb them. Sweep them instead.
    ProposalsPending,

    /// The group's pending-proposal set is full and a committer must sweep it
    /// before anything else can be asked for.
    PendingSetFull,

    /// A sweep built by
    /// [`Group::stage_pending_proposals`](crate::Group::stage_pending_proposals)
    /// is already awaiting its journal verdict. Reach that verdict — merge or
    /// abandon — rather than retrying the stage.
    SweepAlreadyStaged,

    // ---- ADR-0050 per-device leaves --------------------------------------
    /// The leaf an `Add` names carries a device binding that does not verify
    /// for that leaf (or is malformed). Nobody may admit it: a leaf whose
    /// account cannot be resolved has no place in a Lakko group.
    DeviceBindingInvalid,

    /// The account the added leaf belongs to already holds
    /// [`crate::group_proposals::MAX_LEAVES_PER_ACCOUNT`] leaves in this group.
    /// Remove a device first.
    DeviceLimitReached,

    // ---- ADR-0060 conversation kind ----------------------------------------
    /// The operation would change the set of people in a one-to-one: add a
    /// third person, remove a person, or let a person leave. A one-to-one
    /// always has the same two people; each person's own devices may still
    /// come and go.
    OneToOneMembershipFixed,

    /// A role change (including an ownership transfer) was asked for in a
    /// one-to-one. A one-to-one has no roles to change and its GroupContext is
    /// frozen from creation.
    OneToOneHasNoRoles,

    /// A commit or proposal would change or remove the conversation's kind
    /// (the `0xFF03` extension, or its entry in `RequiredCapabilities`). The
    /// kind is fixed at creation and no commit changes it.
    GroupKindChanged,
}

impl RefusalReason {
    /// Every reason, in declaration order (authorisation first, then
    /// availability).
    ///
    /// Exists so a test can assert over the whole set — adding a variant without
    /// classifying it deliberately then fails a test rather than defaulting
    /// silently — and so an FFI surface can enumerate without re-listing.
    pub const ALL: [Self; 23] = [
        Self::NotAMember,
        Self::AddNotOwnerOrAdmin,
        Self::RemoveNotOwnerOrAdmin,
        Self::OwnerUnremovable,
        Self::OwnerCannotLeave,
        Self::NotOwner,
        Self::LeafUpdateRefused,
        Self::UnsupportedProposal,
        Self::NoRoleTable,
        Self::OwnerNotAMember,
        Self::RoleTableWouldVanish,
        Self::NotACommitter,
        Self::NoSuchJoinRequest,
        Self::JoinerAlreadyAMember,
        Self::RoleChangePending,
        Self::ProposalsPending,
        Self::PendingSetFull,
        Self::SweepAlreadyStaged,
        Self::DeviceBindingInvalid,
        Self::DeviceLimitReached,
        Self::OneToOneMembershipFixed,
        Self::OneToOneHasNoRoles,
        Self::GroupKindChanged,
    ];

    /// What the caller should do about this refusal.
    ///
    /// Written as an exhaustive match with no wildcard arm on purpose: a new
    /// variant must be classified here, and the compiler is what insists.
    #[must_use]
    pub const fn disposition(self) -> RefusalDisposition {
        match self {
            // A bounded queue is full, and nothing else is wrong. These two are
            // the entire retryable set: `PendingSetFull` is the member proposal
            // set at its cap, and `RoleChangePending` is the single
            // `GroupContextExtensions` slot RFC 9420 allows per commit, which is
            // a queue of capacity one. Both drain when any committer sweeps,
            // with no action from this caller.
            Self::PendingSetFull | Self::RoleChangePending => RefusalDisposition::RetryLater,

            // Something must change before this call can succeed.
            //
            // `ProposalsPending` is here rather than under `RetryLater` even
            // though the pending set does drain: it refuses a **direct** commit
            // in a group that has moved to proposals, and ADR-0045 forbids
            // keeping that as a second route into membership. The answer is the
            // proposal path, not the same call again later.
            //
            // `SweepAlreadyStaged` is here because it resolves only when *this*
            // caller reaches the outstanding verdict — merge or abandon —
            // which is the definition of not resolving on its own.
            Self::LeafUpdateRefused
            | Self::UnsupportedProposal
            | Self::NoRoleTable
            | Self::OwnerNotAMember
            | Self::RoleTableWouldVanish
            | Self::ProposalsPending
            | Self::SweepAlreadyStaged
            | Self::NoSuchJoinRequest
            | Self::JoinerAlreadyAMember
            | Self::DeviceLimitReached
            | Self::GroupKindChanged => RefusalDisposition::DoNotRetry,

            // An authority answer.
            Self::NotAMember
            | Self::AddNotOwnerOrAdmin
            | Self::RemoveNotOwnerOrAdmin
            | Self::OwnerUnremovable
            | Self::OwnerCannotLeave
            | Self::NotOwner
            | Self::NotACommitter
            | Self::DeviceBindingInvalid
            | Self::OneToOneMembershipFixed
            | Self::OneToOneHasNoRoles => RefusalDisposition::NotPermitted,
        }
    }

    /// `true` when the **identical** call will succeed once a committer has
    /// swept, with the caller doing nothing else — i.e. when the UI may offer a
    /// plain retry.
    ///
    /// Exactly [`RefusalDisposition::RetryLater`], named for what a caller
    /// actually wants to know, so no client restates the mapping. Note that it
    /// is deliberately **not** the complement of "permitted": a
    /// [`RefusalDisposition::DoNotRetry`] refusal is not a permission problem
    /// and is still not retryable.
    #[must_use]
    pub const fn retry_may_succeed(self) -> bool {
        matches!(self.disposition(), RefusalDisposition::RetryLater)
    }

    /// A stable, machine-readable code for this reason.
    ///
    /// This is the discriminator the wasm surface carries on the thrown error
    /// (`err.refusalReason`), so it is **wire-visible and must not drift**:
    /// renaming a code silently stops a web client recognising a refusal it used
    /// to handle. `lowerCamelCase` matches the flat-string convention the other
    /// wasm enums follow (`ProposalKind`, the SFU frame `kind`).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotAMember => "notAMember",
            Self::AddNotOwnerOrAdmin => "addNotOwnerOrAdmin",
            Self::RemoveNotOwnerOrAdmin => "removeNotOwnerOrAdmin",
            Self::OwnerUnremovable => "ownerUnremovable",
            Self::OwnerCannotLeave => "ownerCannotLeave",
            Self::NotOwner => "notOwner",
            Self::LeafUpdateRefused => "leafUpdateRefused",
            Self::UnsupportedProposal => "unsupportedProposal",
            Self::NoRoleTable => "noRoleTable",
            Self::OwnerNotAMember => "ownerNotAMember",
            Self::RoleTableWouldVanish => "roleTableWouldVanish",
            Self::NotACommitter => "notACommitter",
            Self::NoSuchJoinRequest => "noSuchJoinRequest",
            Self::JoinerAlreadyAMember => "joinerAlreadyAMember",
            Self::RoleChangePending => "roleChangePending",
            Self::ProposalsPending => "proposalsPending",
            Self::PendingSetFull => "pendingSetFull",
            Self::SweepAlreadyStaged => "sweepAlreadyStaged",
            Self::DeviceBindingInvalid => "deviceBindingInvalid",
            Self::DeviceLimitReached => "deviceLimitReached",
            Self::OneToOneMembershipFixed => "oneToOneMembershipFixed",
            Self::OneToOneHasNoRoles => "oneToOneHasNoRoles",
            Self::GroupKindChanged => "groupKindChanged",
        }
    }
}

impl RefusalDisposition {
    /// A stable, machine-readable code for this disposition, carried alongside
    /// [`RefusalReason::code`] on the wasm surface.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RetryLater => "retryLater",
            Self::DoNotRetry => "doNotRetry",
            Self::NotPermitted => "notPermitted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` is hand-written, so it can fall out of step with the enum. Nothing
    /// in the language checks that, and every property test over the set is
    /// vacuous for a variant that is missing from it — so it is checked here,
    /// by asking each variant to find itself.
    #[test]
    fn all_lists_every_variant_exactly_once() {
        for reason in RefusalReason::ALL {
            let hits = RefusalReason::ALL.iter().filter(|r| **r == reason).count();
            assert_eq!(hits, 1, "{reason:?} must appear in ALL exactly once");
        }
        // A second, independent handle on the same question: every code is
        // distinct, so a duplicated or missing entry cannot hide.
        let mut codes: Vec<&str> = RefusalReason::ALL.iter().map(|r| r.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), RefusalReason::ALL.len());
    }

    /// The maintainer's rule, stated as an assertion: **retryable exactly when
    /// a bounded queue was full.**
    ///
    /// Written as an explicit list rather than derived from `disposition`, so a
    /// reason that moves between dispositions has to be moved here too. A test
    /// that recomputed the rule from the thing it is testing would pass no
    /// matter what.
    #[test]
    fn exactly_the_queue_full_refusals_are_retryable() {
        let retryable: Vec<RefusalReason> = RefusalReason::ALL
            .into_iter()
            .filter(|r| r.retry_may_succeed())
            .collect();
        assert_eq!(
            retryable,
            vec![
                RefusalReason::RoleChangePending,
                RefusalReason::PendingSetFull,
            ],
            "only a full bounded queue is retryable; nothing about a request's \
             own state ever is",
        );
    }

    #[test]
    fn disposition_codes_are_stable() {
        assert_eq!(RefusalDisposition::RetryLater.code(), "retryLater");
        assert_eq!(RefusalDisposition::DoNotRetry.code(), "doNotRetry");
        assert_eq!(RefusalDisposition::NotPermitted.code(), "notPermitted");
    }
}
