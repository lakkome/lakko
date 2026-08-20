// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS group state machine — wraps OpenMLS's [`MlsGroup`].
//!
//! Provides a Lakko-shaped API on top of OpenMLS:
//!
//! - [`Group::create`] — start a new group with the local identity
//! - [`Group::join_from_welcome`] — join from a Welcome message
//! - [`Group::add_member`] — produce a Commit + Welcome to add a key package
//! - [`Group::remove_member`] — produce a Commit removing a leaf
//! - [`Group::create_application_message`] — encrypt application data
//! - [`Group::process_incoming`] — decrypt / advance the state machine
//! - [`Group::export_secret`] — derive a key for non-MLS uses (SFrame, etc.)
//!
//! Membership changes come in two shapes. The **direct-commit** shape
//! ([`Group::add_member`], [`Group::remove_member`], [`Group::set_group_roles`],
//! [`Group::approve_external_join`]) is the original one and is destined to go
//! (ADR-0045). The **proposal** shape that replaces it is
//! [`Group::propose_add_member`] / [`Group::propose_remove_member`] /
//! [`Group::propose_group_roles`], with one designated owner-or-admin — see
//! [`crate::group_roles::committer_rank`] — turning the pending set into a
//! single Commit via [`Group::commit_pending_proposals`]. The authorisation
//! rule both shapes are judged by lives in [`crate::group_proposals`].
//!
//! Ownership moves with [`Group::transfer_ownership`] /
//! [`Group::propose_ownership_transfer`] (ADR-0022's deferred follow-up), which
//! are validated wrappers over the same role-table rewrite rather than a new
//! commit kind. They are what makes leaving possible at all: an owner may not
//! leave while owner, so without transfer a person was bound to every group they
//! created.

use openmls::extensions::{
    Extension, ExtensionType, Extensions, RequiredCapabilitiesExtension, UnknownExtension,
};
use openmls::framing::{MlsMessageBodyIn, MlsMessageIn, MlsMessageOut, Sender};
use openmls::group::{
    GroupContext, MlsGroup, MlsGroupCreateConfig, MlsGroupJoinConfig, QueuedProposal, StagedCommit,
    StagedWelcome,
};
use openmls::messages::proposals::Proposal;
use openmls::prelude::{KeyPackage, LeafNodeIndex, ProcessedMessageContent, ProtocolVersion};
use openmls_rust_crypto::MemoryStorage;
use openmls_traits::OpenMlsProvider as _;
use tls_codec::{Deserialize as _, Serialize as _};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::group_proposals::{
    authorise, classify, commit_effects, owner_survives, pubkey_at_leaf, ProposalKind,
    ProposalRecord, Proposer,
};
use crate::group_roles::{OutgoingOwner, Role, RoleTable, ROLE_EXTENSION_TYPE};
use crate::identity::Identity;

/// Byte length of a Lakko-minted MLS group id.
///
/// **This value is load-bearing beyond MLS.** A conversation group's
/// `group_id` is the routing key for the Delivery Service journal
/// (`POST /ds/group/{group_id_hex}`), and the DS pins the same width in
/// `services/ds/src/journal.rs`:
///
/// ```text
/// const PSEUDONYM_LEN: usize = 32;
/// const HEX_LEN: usize = PSEUDONYM_LEN * 2;
/// ```
///
/// `decode_pseudonym` rejects anything other than `HEX_LEN` lowercase hex
/// chars with a 400, and no client applies any padding or derivation between
/// [`Group::group_id`] and that path — the raw bytes are hex-encoded as-is.
/// So a group id of any other width cannot be published at all.
///
/// The two constants cannot be tied by a compile-time assertion: `services/ds`
/// deliberately depends on no `lakko-*` crate (it is the opaque zero-trust
/// relay and never parses MLS — invariant #2), and making it depend on
/// `lakko-mls` would pull the entire OpenMLS stack into the server for the
/// sake of one integer. The coupling is instead pinned by
/// `group_id_length_is_stable_across_constructors`, which asserts this width
/// against a locally restated copy of the DS constant, so a change on either
/// side fails a test rather than silently 400-ing every publish.
///
/// OpenMLS's own `GroupId::random` mints **16** bytes, which is why
/// [`Group::create`] does not use it.
pub const GROUP_ID_LEN: usize = 32;

/// Max external-join proposals retained per group awaiting an owner/admin
/// decision (ADR-0027). A proposal arriving past this cap is surfaced but not
/// retained — anti-spam, since a non-member can post these.
const MAX_PENDING_EXTERNAL_JOINS: usize = 64;

/// Max **member** proposals retained per group awaiting an ADR-0045 sweep.
///
/// The sibling of [`MAX_PENDING_EXTERNAL_JOINS`], and needed for the same
/// reason: ADR-0045 decision 2 gives a request no deadline, so the pending set
/// is bounded by nothing but a committer eventually noticing. It is not free to
/// leave unbounded — the set is cloned, cleared and re-stored once per outgoing
/// chat message ([`Group::create_application_message`]), so its size is a
/// per-message cost paid by **every** member of the group, not only by the
/// proposer; it is persisted, so it survives restarts; and every member holds
/// it, because by-reference resolution requires that.
///
/// The rule [`crate::group_proposals::authorise`] leaves open to a plain member
/// is proposing its **own** removal, and nothing stops one member emitting that
/// same proposal N times: the ratchet refuses only a byte-identical replay,
/// while N fresh encryptions of one plaintext proposal all decrypt to the same
/// [`openmls::prelude::ProposalRef`]. That is what
/// [`Group::gate_incoming_proposal`] de-duplicates, and this cap is what bounds
/// the residue of N genuinely distinct requests.
///
/// **At the cap the late proposer pays, never the group.** A proposal arriving
/// into a full set is refused (`stored == false`) rather than evicting one
/// already held — evicting would be strictly worse than the flood, because a
/// commit that references an evicted proposal can no longer be resolved and is
/// refused with `InvalidCommit(MissingProposal)` on the evicting member alone.
/// Refusing the newcomer costs it a re-proposal after the next sweep; evicting
/// an incumbent would cost the whole group its next commit.
const MAX_PENDING_PROPOSALS: usize = 64;

/// A non-member's external-join request retained for an owner/admin decision
/// (ADR-0027, G-3). `joiner_signature_key` is the canonical identity handle the
/// rest of `lakko-mls` keys on (the role table + the contact graph match on
/// it); it doubles as the opaque ref the FFI / UI pass back to
/// [`Group::approve_external_join`] / [`Group::reject_external_join`].
/// `key_package` is the joiner's KeyPackage that an approve adds via the
/// existing, audited [`Group::add_member`] path.
struct PendingExternalJoin {
    joiner_signature_key: Vec<u8>,
    key_package: KeyPackage,
}

/// Lakko MLS conversation group.
///
/// Owns the local view of an MLS group. Per-device state lives in the
/// associated [`Identity`]'s storage provider; this struct is moved /
/// borrowed alongside that identity for every operation.
pub struct Group {
    inner: MlsGroup,
    /// External-join proposals (ADR-0027) awaiting an owner/admin approve /
    /// reject. **In-memory only in v1** — proposals are epoch-bound +
    /// ephemeral, so an app restart drops them and the joiner re-sends;
    /// persistence is a follow-up. Bounded by [`MAX_PENDING_EXTERNAL_JOINS`].
    pending_external_joins: Vec<PendingExternalJoin>,
}

/// Output of [`Group::add_member`].
///
/// - `commit` is broadcast to existing group members so they can advance
///   their state to the new epoch.
/// - `welcome` is delivered to the **newly added** member so they can join.
pub struct MemberAddition {
    /// Commit — distribute to all existing members.
    pub commit: MlsMessageOut,
    /// Welcome — distribute to the newly added member only.
    pub welcome: MlsMessageOut,
}

/// Output of [`Group::commit_pending_proposals`] — the ADR-0045 sweep.
///
/// Mirrors [`MemberAddition`]'s shape so ADR-0040's durable pending-commit
/// outbox wraps it unchanged, with two differences the proposal path forces:
/// the Welcome is **optional** (a sweep that carried no Add produces none), and
/// `swept` names what the commit actually carried so the caller can mark those
/// requests applied.
pub struct ProposalCommit {
    /// Commit — distribute to all existing members.
    pub commit: MlsMessageOut,
    /// Welcome — distribute to the newly added members, when the sweep carried
    /// at least one `Add`. ADR-0040's two-artifact outbox coverage applies.
    pub welcome: Option<MlsMessageOut>,
    /// The proposals this commit carried, in sweep order.
    pub swept: Vec<ProposalRecord>,
}

/// One member's identity snapshot at the current epoch
/// (Phase 1.18h). Returned by [`Group::members`].
///
/// `leaf_index` is the OpenMLS-side leaf position used by
/// `remove_member` to identify which leaf to delete. Stable
/// inside a single epoch; can renumber across a remove/add
/// cycle, so callers MUST NOT cache it past an
/// [`Group::process_incoming`] of a Commit.
///
/// `signature_public_key` is the raw 32-byte Ed25519
/// verifying key the member's identity layer signs with —
/// callers use it to render a fingerprint, look the member up
/// in the contacts cache, or compare against `Identity::signature_public_key`
/// to identify "is this me".
#[derive(Debug, Clone)]
pub struct MemberRecord {
    /// OpenMLS leaf position. `u32` mirrors `LeafNodeIndex`'s
    /// internal repr.
    pub leaf_index: u32,
    /// 32-byte Ed25519 verifying key.
    pub signature_public_key: Vec<u8>,
}

/// Result of [`Group::process_incoming`].
#[derive(Debug)]
pub enum Incoming {
    /// Decrypted application payload, plus the sender's leaf index when the
    /// message came from a group member (`None` for an external sender). The
    /// sender leaf is what the chat receive path needs for ADR-0023's
    /// author-only edit/delete gate + reaction attribution; for non-chat
    /// payloads (DeviceGroup journal, call signaling) it is simply ignored.
    Application {
        /// The decrypted application bytes.
        bytes: Vec<u8>,
        /// The sender's leaf index, or `None` for an external sender.
        sender_leaf: Option<u32>,
    },
    /// A commit was processed and merged into the group state. The local
    /// view advanced to a new epoch.
    Commit,
    /// A standalone member proposal was processed (ADR-0045). No state advance
    /// — a proposal is not epoch-advancing, which is the whole point: two
    /// members proposing concurrently compose instead of forking the loser.
    ///
    /// The record names the proposer's leaf, what was proposed, and whether the
    /// ADR-0045 receipt gate retained it for a later sweep
    /// ([`ProposalRecord::stored`]). `stored == false` is the visible form of a
    /// refusal — see that field's documentation for why a refused proposal is
    /// data rather than an error.
    Proposal(ProposalRecord),
    /// A non-member's external join proposal was processed and (unless the
    /// per-group cap is hit) retained for an owner/admin decision (ADR-0027,
    /// G-3). `joiner_signature_key` is the prospective joiner's leaf signature
    /// public key — the stable handle the caller passes to
    /// [`Group::approve_external_join`] / [`Group::reject_external_join`], and
    /// the key the role table / contact graph resolve to a display name.
    ExternalJoinProposal {
        /// The prospective joiner's leaf signature public key.
        joiner_signature_key: Vec<u8>,
    },
}

impl Group {
    /// Create a new MLS group with the given identity as the founding member.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for any OpenMLS-side failure.
    pub fn create(identity: &Identity) -> Result<Self> {
        // ADR-0022 (owner-bootstrap option A) — a conversation group is
        // role-enabled from creation with the creator as owner. The initial
        // GroupContext carries the `0xFF02` role table (owner = creator, no
        // admins) plus the required-capabilities declaration that lets a later
        // `set_group_roles` commit rewrite it (promote / demote). Seeding the
        // owner at creation closes the bootstrap window — there is never a
        // moment when the group has no owner for a member to claim.
        let owner: [u8; 32] = identity
            .signature_public_key()
            .try_into()
            .map_err(|_| Error::Group("identity signature key is not 32 bytes".into()))?;
        let initial = RoleTable::owner_only(owner);
        let exts = Extensions::from_vec(vec![
            role_required_capabilities(),
            Extension::Unknown(ROLE_EXTENSION_TYPE, UnknownExtension(initial.encode())),
        ])
        .map_err(|e| Error::Group(format!("build role extensions: {e:?}")))?;
        let cfg = MlsGroupCreateConfig::builder()
            .ciphersuite(identity.ciphersuite())
            // Embed the ratchet tree in Welcomes so joiners can reconstruct
            // group state without an out-of-band tree fetch. We may revisit
            // this for very large groups (Phase 4 channels) where the tree
            // can dominate Welcome size.
            .use_ratchet_tree_extension(true)
            // Advertise support for all Lakko GroupContext extensions (so the
            // role table can be carried + later rewritten).
            .capabilities(crate::device_group::lakko_leaf_capabilities(
                identity.ciphersuite(),
            ))
            .with_group_context_extensions(exts)
            .build();

        // Mint the group id ourselves rather than letting OpenMLS pick one:
        // `MlsGroup::new` falls through to `GroupId::random`, which is only
        // **16** bytes (openmls-0.8.1 `group/mod.rs`, via `builder.rs`
        // `.unwrap_or_else(|| GroupId::random(provider.rand()))`). The group
        // id is the DS journal routing key and the DS requires exactly
        // `GROUP_ID_LEN` — see that constant for the full coupling.
        //
        // Randomness goes through `lakko_crypto::random` per that module's
        // policy ("all randomness ... MUST come through this module").
        let mut group_id = [0u8; GROUP_ID_LEN];
        lakko_crypto::random::fill(&mut group_id);

        Self::create_inner(identity, &cfg, &group_id, "create")
    }

    /// Shared tail of [`Group::create`] and [`Group::create_with_id`]: build
    /// the `MlsGroup` at an explicit `group_id`.
    ///
    /// Both constructors route through OpenMLS's `new_with_group_id` — they
    /// differ only in the config they pass (`create` seeds the ADR-0022 role
    /// table; `create_with_id` deliberately does not) and in where the id
    /// comes from. Keeping the construction call in one place means the id
    /// width cannot drift between them again.
    fn create_inner(
        identity: &Identity,
        cfg: &MlsGroupCreateConfig,
        group_id: &[u8],
        ctx: &str,
    ) -> Result<Self> {
        use openmls::group::GroupId;

        let inner = MlsGroup::new_with_group_id(
            &identity.provider,
            &identity.signature_keys,
            cfg,
            GroupId::from_slice(group_id),
            identity.credential.clone(),
        )
        .map_err(|e| Error::Group(format!("{ctx}: {e:?}")))?;

        Ok(Self::from_mls_group(inner))
    }

    /// Create a new MLS group with a **caller-supplied** group id.
    ///
    /// Lakko's default [`Group::create`] mints a random
    /// [`GROUP_ID_LEN`]-byte group id at creation time. That is the
    /// right shape for 1×1 chats / multi-party calls / DeviceGroup
    /// where the group is uniquely identified by a server-side
    /// opaque pseudonym.
    ///
    /// **Note on width:** the id supplied here is used verbatim, so a
    /// caller that routes the resulting group through the DS journal
    /// must supply exactly [`GROUP_ID_LEN`] bytes. `PublisherGroup`
    /// supplies a longer, deterministic id (a domain prefix plus the
    /// channel id) and is addressed by `channel_id` on the channels
    /// service rather than by `group_id` on the DS.
    ///
    /// For **PublisherGroup** (G4-2-mls-persistence, ADR-0010
    /// Tier-1) the publisher needs to round-trip the same group
    /// state across process restarts keyed by `channel_id` — so
    /// the group id MUST be deterministic from the channel id.
    /// This variant supplies the same `MlsGroup` config as
    /// [`Group::create`] but routes through OpenMLS's
    /// `new_with_group_id` constructor instead of the
    /// random-id `new`.
    ///
    /// # Errors
    ///
    /// Same shape as [`Group::create`]: [`Error::Group`] on any
    /// OpenMLS-side failure.
    pub fn create_with_id(identity: &Identity, group_id: &[u8]) -> Result<Self> {
        let cfg = MlsGroupCreateConfig::builder()
            .ciphersuite(identity.ciphersuite())
            .use_ratchet_tree_extension(true)
            .build();

        Self::create_inner(identity, &cfg, group_id, "create_with_id")
    }

    /// Join an existing group from a Welcome message.
    ///
    /// `welcome_bytes` is the TLS-encoded Welcome (extracted by the sender
    /// from the [`MemberAddition`] returned by [`Group::add_member`]).
    ///
    /// The join config sets `use_ratchet_tree_extension(true)` so the joined
    /// group's own future Welcomes (when this member later acts as the
    /// committer adding someone else) also carry the embedded ratchet tree.
    /// Without this flag the joined group inherits the OpenMLS default
    /// (false), which means a chain of welcome-then-add propagation
    /// fails with `MissingRatchetTree` at the second-hop join — load-bearing
    /// for the link-call owner-handover path where the founder leaves and
    /// the owner (who joined via welcome) becomes the new committer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Codec`] for malformed wire data, [`Error::Unsupported`]
    /// if the message is not a Welcome, or [`Error::Group`] on OpenMLS
    /// failure (e.g. signature mismatch, ciphersuite mismatch).
    pub fn join_from_welcome(welcome_bytes: &[u8], identity: &Identity) -> Result<Self> {
        let mls_in = decode_message(welcome_bytes)?;
        let MlsMessageBodyIn::Welcome(welcome) = mls_in.extract() else {
            return Err(Error::Unsupported("expected Welcome message"));
        };

        let cfg = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        let staged = StagedWelcome::new_from_welcome(&identity.provider, &cfg, welcome, None)
            .map_err(|e| Error::Group(format!("staged welcome: {e:?}")))?;
        let inner = staged
            .into_group(&identity.provider)
            .map_err(|e| Error::Group(format!("welcome -> group: {e:?}")))?;

        Ok(Self::from_mls_group(inner))
    }

    /// **A direct commit must never carry a proposal it did not ask for.**
    ///
    /// Every OpenMLS entry point the direct-commit paths below are built on —
    /// `add_members`, `remove_members`, `self_update`,
    /// `update_group_context_extensions` — builds its commit with
    /// `consume_proposal_store` left at its default `true` and the proposal
    /// filter hardcoded to `|_| true` (`openmls-0.8.1`
    /// `group/mls_group/{membership,updates}.rs`). Each therefore sweeps the
    /// **entire** pending set alongside the one change it was asked for.
    ///
    /// Before ADR-0045 that was harmless, because Lakko stored no proposals and
    /// the set was always empty. ADR-0045's receipt gate created the set, and
    /// with it two failure modes on paths that predate it and know nothing
    /// about it:
    ///
    /// * **A silent fork.** An admin's `remove_member` absorbs a pending
    ///   ownership transfer, producing a commit that
    ///   [`crate::group_proposals::owner_survives`] refuses on every peer — but
    ///   the direct paths merge **eagerly**, so its author is already at N+1
    ///   and sits there alone, permanently. That is precisely the G-38
    ///   signature ADR-0045 exists to erase, reintroduced through the back
    ///   door.
    /// * **A member with no Welcome.** An `add_member` that also sweeps in
    ///   somebody else's `Add` gives that person a leaf, while the caller
    ///   routes the single Welcome to the one person it meant to add. The
    ///   swept-in member is in the roster and cannot join.
    ///
    /// So a direct commit is **refused** while anything is pending, rather than
    /// silently widened. Refusing is the right shape rather than filtering the
    /// store out of the commit, for two reasons. First, merging any commit
    /// empties the proposal store on every member (`staged_commit.rs`, both
    /// merge arms) whatever `consume_proposal_store` said, so a direct commit
    /// that excluded the pending set would still **discard** it group-wide —
    /// trading a silent absorb for a silent loss. Second, ADR-0045 forbids a
    /// permanent dual path into membership: a group that has moved to proposals
    /// should sweep, and this is what says so out loud.
    ///
    /// **Nothing that legitimately uses a direct commit today is affected.**
    /// The pending set is only ever written for a group carrying an ADR-0022
    /// role table — [`Self::gate_incoming_proposal`] returns early without
    /// storing when [`Self::group_roles`] is `None`, [`Self::process_incoming`]
    /// and [`Self::process_incoming_gated`] store nothing at all, and every
    /// `propose_*` refuses without a table. So `DeviceGroup`,
    /// `PublisherGroup`, call groups and every other
    /// [`Group::create_with_id`] group has a permanently empty set and this
    /// guard is a counted-zero no-op for them.
    fn refuse_if_proposals_pending(&self, op: &str) -> Result<()> {
        let pending = self.inner.pending_proposals().count();
        if pending == 0 {
            return Ok(());
        }
        Err(Error::Group(format!(
            "{op} refused: {pending} membership proposal(s) are pending in this group, and a \
             direct commit would silently absorb them (ADR-0045) — sweep them with \
             commit_pending_proposals / stage_pending_proposals instead"
        )))
    }

    /// Add a member by their published [`KeyPackage`].
    ///
    /// Consumes one of the new member's published KeyPackages on success.
    /// The local group state is advanced to the new epoch atomically.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] when a member proposal is pending (see
    /// [`Self::refuse_if_proposals_pending`]), or for any OpenMLS-side failure
    /// (validation, signature, storage).
    pub fn add_member(
        &mut self,
        identity: &Identity,
        key_package: KeyPackage,
    ) -> Result<MemberAddition> {
        self.refuse_if_proposals_pending("add_member")?;
        let (commit, welcome, _group_info) = self
            .inner
            .add_members(&identity.provider, &identity.signature_keys, &[key_package])
            .map_err(|e| Error::Group(format!("add_members: {e:?}")))?;

        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;

        Ok(MemberAddition { commit, welcome })
    }

    /// Commit a self-update with default leaf-node parameters.
    /// Advances the group epoch by one. Subsequent calls to
    /// [`Group::export_secret`] derive against the new epoch
    /// and therefore produce a different secret — that is
    /// exactly the rotation primitive
    /// [`crate::publisher_group::PublisherGroup::rotate`]
    /// surfaces for the Phase 4 channels publisher path
    /// (24 h / N-posts scheduled rotation).
    ///
    /// Returns the wire-encoded commit. Welcomes are not
    /// emitted by a no-op self_update — the bundle's
    /// `welcome()` is `None` here per OpenMLS semantics.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] when a member proposal is pending (see
    /// [`Self::refuse_if_proposals_pending`]), or for any OpenMLS-side failure.
    pub fn self_update(&mut self, identity: &Identity) -> Result<Vec<u8>> {
        use openmls::prelude::LeafNodeParameters;
        self.refuse_if_proposals_pending("self_update")?;
        let bundle = self
            .inner
            .self_update(
                &identity.provider,
                &identity.signature_keys,
                LeafNodeParameters::default(),
            )
            .map_err(|e| Error::Group(format!("self_update: {e:?}")))?;
        let commit_msg = bundle.into_commit();
        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;
        encode_message(&commit_msg)
    }

    /// Add a member from a wire-format `KeyPackageIn`.
    ///
    /// Convenience wrapper that deserializes the bytes via
    /// [`crate::key_package::deserialize`], validates the package against
    /// the active MLS protocol version and ciphersuite, then delegates to
    /// [`Group::add_member`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyPackage`] for validation failures.
    pub fn add_member_from_wire(
        &mut self,
        identity: &Identity,
        key_package_bytes: &[u8],
    ) -> Result<MemberAddition> {
        let kp_in = crate::key_package::deserialize(key_package_bytes)?;
        let validated = kp_in
            .validate(identity.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| Error::KeyPackage(format!("validate: {e:?}")))?;
        self.add_member(identity, validated)
    }

    /// Remove a member identified by their leaf index.
    ///
    /// Indices come from the receiving member's own group view (e.g. via
    /// [`Group::own_leaf_index`]) or from `MlsGroup::members()` once we
    /// surface that iterator.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] when a member proposal is pending (see
    /// [`Self::refuse_if_proposals_pending`]), or for any OpenMLS-side failure.
    pub fn remove_member(
        &mut self,
        identity: &Identity,
        leaf: LeafNodeIndex,
    ) -> Result<MlsMessageOut> {
        self.refuse_if_proposals_pending("remove_member")?;
        let (commit, _welcome_for_external_joiners, _group_info) = self
            .inner
            .remove_members(&identity.provider, &identity.signature_keys, &[leaf])
            .map_err(|e| Error::Group(format!("remove_members: {e:?}")))?;

        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;

        Ok(commit)
    }

    /// Replace this group's GroupContext extensions, returning the commit to
    /// broadcast (G-13, ADR-0015). The commit carries a
    /// `GroupContextExtensions` proposal; every member that processes it sees
    /// the new extensions in their authenticated GroupContext. Merges the
    /// pending commit locally, mirroring [`Self::remove_member`].
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when a member proposal is pending (see
    /// [`Self::refuse_if_proposals_pending`]), or on the OpenMLS update / merge
    /// (e.g. a member whose leaf capabilities do not advertise an added
    /// extension type).
    pub fn set_group_context_extensions(
        &mut self,
        identity: &Identity,
        extensions: Extensions<GroupContext>,
    ) -> Result<MlsMessageOut> {
        self.refuse_if_proposals_pending("set_group_context_extensions")?;
        let (commit, _welcome, _group_info) = self
            .inner
            .update_group_context_extensions(
                &identity.provider,
                extensions,
                &identity.signature_keys,
            )
            .map_err(|e| Error::Group(format!("update_group_context_extensions: {e:?}")))?;

        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;

        Ok(commit)
    }

    /// Read a custom (`Unknown`) GroupContext extension's payload by type id,
    /// if present. Used to read DeviceGroup policy flags from authenticated
    /// group state (G-13, ADR-0015).
    #[must_use]
    pub fn group_context_extension(&self, type_id: u16) -> Option<Vec<u8>> {
        self.inner
            .extensions()
            .unknown(type_id)
            .map(|e| e.0.clone())
    }

    /// The group's ADR-0022 admin role table, if it has opted into the role
    /// policy — i.e. the `0xFF02` GroupContext extension is present and
    /// well-formed. `None` means an **open** group: no role policy, any member
    /// may add / remove (today's default, and every 1×1 / legacy group). A
    /// malformed `0xFF02` body also reads as `None` (fail-open *for the read
    /// accessor* — the enforcing apply path treats a malformed-but-present
    /// table conservatively; see `process_incoming_verifying_roles`).
    #[must_use]
    pub fn group_roles(&self) -> Option<RoleTable> {
        self.group_context_extension(ROLE_EXTENSION_TYPE)
            .and_then(|body| RoleTable::decode(&body).ok())
    }

    /// This member's role in the group, or `None` when the group has no role
    /// table (open group). Resolved by matching our own account-level signature
    /// key against the table — the single-account-leaf model ADR-0022 v1 assumes
    /// (Finding 1). A member not named in the table is [`Role::Member`].
    #[must_use]
    pub fn own_role(&self, identity: &Identity) -> Option<Role> {
        self.group_roles()
            .map(|table| table.role_of(identity.signature_public_key()))
    }

    /// Retain a non-member's external-join proposal for an owner/admin decision
    /// (ADR-0027, G-3). Validates it is genuinely an `Add` from a
    /// `NewMemberProposal` sender (defence in depth — a member must not smuggle
    /// a different proposal through this path), extracts the joiner's signature
    /// key + KeyPackage, de-dups by signature key, and retains it under the
    /// [`MAX_PENDING_EXTERNAL_JOINS`] cap. The event is always surfaced even
    /// when the cap drops the retention.
    fn retain_external_join(&mut self, qp: &QueuedProposal) -> Result<Incoming> {
        if !matches!(qp.sender(), Sender::NewMemberProposal) {
            return Err(Error::Group(
                "external join proposal from a non-new-member sender refused".into(),
            ));
        }
        let Proposal::Add(add) = qp.proposal() else {
            return Err(Error::Group(
                "external join proposal carried a non-Add proposal refused".into(),
            ));
        };
        let key_package = add.key_package().clone();
        let joiner_signature_key = key_package.leaf_node().signature_key().as_slice().to_vec();

        let already_pending = self
            .pending_external_joins
            .iter()
            .any(|p| p.joiner_signature_key == joiner_signature_key);
        if !already_pending && self.pending_external_joins.len() < MAX_PENDING_EXTERNAL_JOINS {
            self.pending_external_joins.push(PendingExternalJoin {
                joiner_signature_key: joiner_signature_key.clone(),
                key_package,
            });
        }
        Ok(Incoming::ExternalJoinProposal {
            joiner_signature_key,
        })
    }

    /// The signature key of every external-join proposal currently awaiting a
    /// decision (ADR-0027). Each is the handle for
    /// [`approve_external_join`](Self::approve_external_join) /
    /// [`reject_external_join`](Self::reject_external_join).
    #[must_use]
    pub fn pending_external_joins(&self) -> Vec<Vec<u8>> {
        self.pending_external_joins
            .iter()
            .map(|p| p.joiner_signature_key.clone())
            .collect()
    }

    /// Approve a retained external-join proposal — adds the joiner to the group
    /// (ADR-0027, G-3). `proposal_ref` is the joiner's signature key from
    /// [`Incoming::ExternalJoinProposal`] / [`pending_external_joins`](Self::pending_external_joins).
    ///
    /// **Role-gated** (ADR-0022): on a group carrying a role table only an owner
    /// or admin may approve; a non-admin is refused with no commit produced
    /// (fail-closed, invariant #4). A legacy group with no table stays open.
    /// Approving reuses the audited [`add_member`](Self::add_member) path —
    /// committing the joiner's external `Add` is equivalent to adding their
    /// KeyPackage — so it returns the in-group Commit + the joiner's Welcome;
    /// peers re-verify the resulting `Add`'s committer role on receipt
    /// (`process_incoming_verifying_roles`), so a forged local approve cannot
    /// take effect on them either.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] if the caller is not owner/admin, if a member proposal
    /// is pending (see [`Self::refuse_if_proposals_pending`] — checked here
    /// rather than only inside [`Self::add_member`] so a refusal does not
    /// consume the retained request), if no pending proposal matches
    /// `proposal_ref`, or on an OpenMLS add failure.
    pub fn approve_external_join(
        &mut self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<MemberAddition> {
        self.refuse_if_proposals_pending("external join approve")?;
        if let Some(table) = self.group_roles() {
            if !table.can_administer(identity.signature_public_key()) {
                return Err(Error::Group(
                    "external join approve refused: not owner or admin".into(),
                ));
            }
        }
        let pos = self
            .pending_external_joins
            .iter()
            .position(|p| p.joiner_signature_key == proposal_ref)
            .ok_or_else(|| Error::Group("no pending external join for that key".into()))?;
        let pending = self.pending_external_joins.remove(pos);
        self.add_member(identity, pending.key_package)
    }

    /// Reject a retained external-join proposal — discards it locally without
    /// adding the joiner or advancing the epoch (ADR-0027). Local-only (no
    /// commit, no peer effect); the clients gate the affordance on `own_role`.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] if no pending proposal matches `proposal_ref`.
    pub fn reject_external_join(&mut self, proposal_ref: &[u8]) -> Result<()> {
        let pos = self
            .pending_external_joins
            .iter()
            .position(|p| p.joiner_signature_key == proposal_ref)
            .ok_or_else(|| Error::Group("no pending external join for that key".into()))?;
        self.pending_external_joins.remove(pos);
        Ok(())
    }

    /// Run the checks every peer will run on a role-table rewrite, locally and
    /// **before** any MLS state is touched.
    ///
    /// [`Self::transfer_ownership`] has validated up front since it landed
    /// ([`Self::plan_ownership_transfer`]), while [`Self::set_group_roles`] —
    /// the path transfer is built on, and the one a client actually calls to
    /// promote or demote an admin — took any table the caller handed it and
    /// **merged eagerly**. So the two disagreed about the same commit: a table
    /// naming an owner no leaf holds was refused by name through one door and,
    /// through the other, built, merged locally, published, and then refused by
    /// every peer's [`crate::group_proposals::owner_survives`] — leaving its
    /// author alone at N+1. Self-forking on a commit the caller could have been
    /// told about is the asymmetry this removes; the peer gate stays the
    /// enforcement, and both checks below are already enforced there.
    ///
    /// The owner check is deliberately the **pre-merge** table (ADR-0022
    /// Finding 2): a rewrite is judged against the authority the caller
    /// inherits, never the authority it is writing — which is exactly what lets
    /// an ownership transfer pass, since the transferor is still the owner at
    /// the moment it mints one.
    fn refuse_unenforceable_role_table(
        &self,
        identity: &Identity,
        roles: &RoleTable,
    ) -> Result<()> {
        let table = self.role_table_or_refuse("role-table change")?;
        if !table.is_owner(identity.signature_public_key()) {
            return Err(Error::Group(
                "role-table change refused: this device is not the group owner".into(),
            ));
        }
        if !self
            .members()
            .iter()
            .any(|m| m.signature_public_key.as_slice() == roles.owner().as_slice())
        {
            return Err(Error::Group(
                "role-table change refused: the named owner is not a member of this group".into(),
            ));
        }
        Ok(())
    }

    /// Rewrite the group's ADR-0022 role table (promote / demote admins).
    /// Produces a `GroupContextExtensions` commit the caller broadcasts to the
    /// group journal.
    ///
    /// **Authorisation is enforced on the RECEIVING side** by
    /// [`process_incoming_verifying_roles`](Self::process_incoming_verifying_roles)
    /// — that is where the boundary is, and it does not trust this method. What
    /// this method adds is the local half: it refuses up front, before touching
    /// any MLS state, exactly what a peer would refuse afterwards, so a caller
    /// gets an error instead of a commit nobody will accept. See
    /// [`Self::refuse_unenforceable_role_table`].
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group carries no role table, when this device
    /// is not the owner, when the new table names an owner who is not a member
    /// of the group, when a member proposal is pending (see
    /// [`Self::refuse_if_proposals_pending`]), or on any OpenMLS-side failure.
    pub fn set_group_roles(&mut self, identity: &Identity, roles: &RoleTable) -> Result<Vec<u8>> {
        self.refuse_unenforceable_role_table(identity, roles)?;
        // `set_group_context_extensions` replaces the whole extension set, so
        // re-include the required-capabilities declaration alongside the new
        // table (the role type must stay in required_capabilities).
        let exts = Extensions::from_vec(vec![
            role_required_capabilities(),
            Extension::Unknown(ROLE_EXTENSION_TYPE, UnknownExtension(roles.encode())),
        ])
        .map_err(|e| Error::Group(format!("build role extensions: {e:?}")))?;
        let commit = self.set_group_context_extensions(identity, exts)?;
        encode_message(&commit)
    }

    // ========================================================================
    // ADR-0045 — proposal-based membership changes
    //
    // A member PROPOSES; one designated owner-or-admin SWEEPS the pending set
    // into a single Commit. Because a proposal does not advance the epoch, two
    // members acting concurrently produce two proposals that compose, instead
    // of two commits framed at the same epoch of which one is rejected by
    // everyone while its author has already merged it locally (G-38 residual 1).
    //
    // The four direct-commit paths above — `add_member`, `remove_member`,
    // `set_group_roles`, `approve_external_join` — are DESTINED TO GO once the
    // three clients have migrated onto this API (ADR-0045 sprints S4-S7). They
    // are deliberately left in place and unchanged for now so the clients keep
    // building; ADR-0045 forbids keeping them as a permanent fallback, because
    // a second ungated route into group membership is the hazard rebuilt.
    // ========================================================================

    /// The role table this group enforces, or an error naming why a
    /// proposal-based operation cannot run without one.
    ///
    /// A group with no `0xFF02` table (legacy / `create_with_id`, e.g. a
    /// `PublisherGroup` or a call group) has no owner, therefore no committer,
    /// therefore nothing to sweep a proposal — ADR-0045 leaves such a group on
    /// direct commits rather than inventing a committer for it. Refusing here
    /// keeps a proposal from being published into a group where nobody would
    /// ever commit it.
    fn role_table_or_refuse(&self, op: &str) -> Result<RoleTable> {
        self.group_roles().ok_or_else(|| {
            Error::Group(format!(
                "{op} refused: this group carries no role table, so it has no committer"
            ))
        })
    }

    /// Run the ADR-0045 rule locally for something *this* device is about to
    /// propose. A courtesy check that keeps our own store clean and gives the
    /// user an immediate error; the enforcing copies are the peers' receipt gate
    /// and commit gate, which do not trust us.
    ///
    /// It also applies the [`MAX_PENDING_PROPOSALS`] cap. That one is not a
    /// courtesy: an honest device must not hand its peers a proposal their
    /// receipt gates would refuse for being over the cap, because it would then
    /// hold a proposal nobody else does and sweep a commit they cannot resolve.
    /// The cap is the same number on both sides for exactly that reason.
    fn authorise_own_proposal(&self, identity: &Identity, kind: &ProposalKind) -> Result<()> {
        let table = self.role_table_or_refuse("proposal")?;
        if self.inner.pending_proposals().count() >= MAX_PENDING_PROPOSALS {
            return Err(Error::Group(format!(
                "proposal refused: this group already holds {MAX_PENDING_PROPOSALS} pending \
                 proposals — a committer must sweep them first"
            )));
        }
        let members = self.members();
        let proposer = Proposer {
            leaf: self.own_leaf_index().u32(),
            pubkey: identity.signature_public_key(),
        };
        authorise(&table, &members, Some(proposer), kind)
    }

    /// Propose adding a member by their published [`KeyPackage`] (ADR-0045).
    ///
    /// Returns the wire-encoded Proposal to publish to the group journal. The
    /// proposal is also retained in this device's own pending set, so a
    /// committer running on this device sweeps it like any other. **The epoch
    /// does not advance** — nothing to merge, nothing to roll back, and no
    /// Welcome yet: the Welcome is produced by whichever device commits.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group has no role table, when this device's
    /// role does not authorise an add, or on any OpenMLS-side failure.
    pub fn propose_add_member(
        &mut self,
        identity: &Identity,
        key_package: KeyPackage,
    ) -> Result<Vec<u8>> {
        let kind = ProposalKind::Add {
            signature_key: key_package.leaf_node().signature_key().as_slice().to_vec(),
        };
        self.authorise_own_proposal(identity, &kind)?;
        let (proposal, _proposal_ref) = self
            .inner
            .propose_add_member(&identity.provider, &identity.signature_keys, &key_package)
            .map_err(|e| Error::Group(format!("propose_add_member: {e:?}")))?;
        encode_message(&proposal)
    }

    /// Propose adding a member from a wire-format `KeyPackageIn` — the
    /// proposal-path twin of [`Group::add_member_from_wire`].
    ///
    /// # Errors
    ///
    /// [`Error::KeyPackage`] for validation failures, otherwise as
    /// [`Group::propose_add_member`].
    pub fn propose_add_member_from_wire(
        &mut self,
        identity: &Identity,
        key_package_bytes: &[u8],
    ) -> Result<Vec<u8>> {
        let kp_in = crate::key_package::deserialize(key_package_bytes)?;
        let validated = kp_in
            .validate(identity.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| Error::KeyPackage(format!("validate: {e:?}")))?;
        self.propose_add_member(identity, validated)
    }

    /// Propose removing the member at `leaf` (ADR-0045).
    ///
    /// Naming **your own** leaf is how a member leaves, and needs no role; any
    /// other leaf needs owner or admin, and the owner's leaf is refused for
    /// everyone. Returns the wire-encoded Proposal to publish.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group has no role table, when this device's
    /// role does not authorise the removal, or on any OpenMLS-side failure.
    pub fn propose_remove_member(
        &mut self,
        identity: &Identity,
        leaf: LeafNodeIndex,
    ) -> Result<Vec<u8>> {
        let kind = ProposalKind::Remove {
            target_leaf: leaf.u32(),
        };
        self.authorise_own_proposal(identity, &kind)?;
        let (proposal, _proposal_ref) = self
            .inner
            .propose_remove_member(&identity.provider, &identity.signature_keys, leaf)
            .map_err(|e| Error::Group(format!("propose_remove_member: {e:?}")))?;
        encode_message(&proposal)
    }

    /// Propose rewriting the ADR-0022 role table (ADR-0045) — the proposal-path
    /// twin of [`Group::set_group_roles`]. Owner-only, enforced locally here and
    /// again by every peer's receipt gate and commit gate.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group has no role table, when this device is
    /// not the owner, when the new table names an owner who is not a member of
    /// the group, or on any OpenMLS-side failure.
    pub fn propose_group_roles(
        &mut self,
        identity: &Identity,
        roles: &RoleTable,
    ) -> Result<Vec<u8>> {
        self.authorise_own_proposal(identity, &ProposalKind::RoleChange)?;
        // Same local pre-check as the direct path. A proposal advances nothing,
        // so this cannot self-fork — but a table naming a non-member owner is
        // one every sweep excludes (`sweep_keeps`), so publishing it would only
        // occupy a slot in every member's pending set until someone noticed.
        self.refuse_unenforceable_role_table(identity, roles)?;
        // As in `set_group_roles`: a GroupContextExtensions proposal replaces
        // the WHOLE extension set, so the required-capabilities declaration has
        // to be re-stated alongside the new table.
        let exts = Extensions::from_vec(vec![
            role_required_capabilities(),
            Extension::Unknown(ROLE_EXTENSION_TYPE, UnknownExtension(roles.encode())),
        ])
        .map_err(|e| Error::Group(format!("build role extensions: {e:?}")))?;
        let (proposal, _proposal_ref) = self
            .inner
            .propose_group_context_extensions(&identity.provider, exts, &identity.signature_keys)
            .map_err(|e| Error::Group(format!("propose_group_context_extensions: {e:?}")))?;
        encode_message(&proposal)
    }

    // ========================================================================
    // Ownership transfer (ADR-0022's deferred follow-up)
    //
    // ADR-0022 fixed the owner at creation and deferred transfer; ADR-0045 then
    // made the consequence permanent by refusing an owner `Leave` fail-closed,
    // so a person was bound to every group they created. Transfer is the way
    // out, and it is deliberately NOT a new commit type: it is the ordinary
    // owner-only role-table rewrite with the owner slot moved, which means it
    // rides the audited `set_group_roles` / `propose_group_roles` paths and is
    // judged by the existing gate against the PRE-merge table — where the
    // transferor is still the owner. Nothing about who may mint one changes.
    //
    // What is new is the safety the raw rewrite never had: a role table names
    // 32-byte keys, so `set_group_roles` could always name an owner that no leaf
    // holds, permanently un-administrable. `owner_survives` now refuses that on
    // every peer, and the two methods below refuse it locally with a clear
    // error before any MLS state is touched.
    // ========================================================================

    /// Validate an ownership transfer and produce the role table it installs.
    ///
    /// Runs entirely before any OpenMLS call, so every refusal here leaves the
    /// group exactly as it was — no staged commit, no epoch advance, nothing to
    /// roll back (ADR-0040).
    fn plan_ownership_transfer(
        &self,
        identity: &Identity,
        new_owner: &[u8],
        outgoing: OutgoingOwner,
    ) -> Result<RoleTable> {
        let table = self.role_table_or_refuse("ownership transfer")?;
        if !table.is_owner(identity.signature_public_key()) {
            return Err(Error::Group(
                "ownership transfer refused: this device is not the group owner".into(),
            ));
        }
        let new_owner: [u8; 32] = new_owner.try_into().map_err(|_| {
            Error::Group("ownership transfer refused: owner key is not 32 bytes".into())
        })?;
        if table.is_owner(&new_owner) {
            return Err(Error::Group(
                "ownership transfer refused: that member is already the owner".into(),
            ));
        }
        // The rule the whole feature rests on. An owner outside the group can
        // never be replaced (a role change is owner-only) and can never be
        // removed (the owner's leaf is unremovable), so the group would be
        // permanently un-administrable. Refused here for a clear error, and
        // again by every peer's commit gate, which is where it is enforced.
        if !self
            .members()
            .iter()
            .any(|m| m.signature_public_key == new_owner)
        {
            return Err(Error::Group(
                "ownership transfer refused: the named owner is not a member of this group".into(),
            ));
        }
        Ok(table.with_owner_transferred(new_owner, outgoing))
    }

    /// Hand the group's owner slot to `new_owner`, as a direct commit
    /// (ADR-0022's deferred ownership-transfer follow-up).
    ///
    /// `new_owner` is the member's Ed25519 signature public key — the same
    /// stable handle [`MemberRecord::signature_public_key`] reports and the role
    /// table keys on, never a leaf index. `outgoing` decides what becomes of
    /// **this** device's own role; see [`OutgoingOwner`], whose default is
    /// [`OutgoingOwner::Admin`].
    ///
    /// **Owner-only, and irreversible.** Only the owner may transfer, because a
    /// role change is owner-only and transfer is a role change; anything weaker
    /// would be a coup, since an admin able to transfer could name itself. And
    /// once the commit lands the transferor has no way back: only the new owner
    /// can rewrite the table. Nothing here recovers a *lost* owner either —
    /// minting the commit needs the owner's key — which is ADR-0022's separate,
    /// still-open admin-quorum recovery question.
    ///
    /// Transfer is what makes leaving possible: an owner may not leave while
    /// owner, but the former owner is an ordinary member and may. The two are
    /// separate commits by construction — see
    /// [`crate::group_proposals::authorise`].
    ///
    /// Returns the wire-encoded commit to broadcast. Prefer
    /// [`Group::propose_ownership_transfer`] in a group that has moved to the
    /// ADR-0045 proposal path.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group carries no role table, when this device
    /// is not the owner, when `new_owner` is already the owner or is not a
    /// member of the group, or on any OpenMLS-side failure.
    pub fn transfer_ownership(
        &mut self,
        identity: &Identity,
        new_owner: &[u8],
        outgoing: OutgoingOwner,
    ) -> Result<Vec<u8>> {
        let table = self.plan_ownership_transfer(identity, new_owner, outgoing)?;
        self.set_group_roles(identity, &table)
    }

    /// Propose handing the group's owner slot to `new_owner` (ADR-0045) — the
    /// proposal-path twin of [`Group::transfer_ownership`], with identical
    /// authorisation and identical refusals.
    ///
    /// The proposal is judged, by this device and by every peer, against the
    /// **pre-merge** table in which this device is still the owner, so it needs
    /// no new rule: it is a [`ProposalKind::RoleChange`], which is owner-only.
    /// Any owner-or-admin may sweep it, including the incoming owner.
    ///
    /// # Errors
    ///
    /// As [`Group::transfer_ownership`].
    pub fn propose_ownership_transfer(
        &mut self,
        identity: &Identity,
        new_owner: &[u8],
        outgoing: OutgoingOwner,
    ) -> Result<Vec<u8>> {
        let table = self.plan_ownership_transfer(identity, new_owner, outgoing)?;
        self.propose_group_roles(identity, &table)
    }

    /// Every proposal currently in this device's pending set, in the order
    /// OpenMLS holds them (ADR-0045).
    ///
    /// This is the committer's "N changes waiting to apply" surface and the
    /// proposer's "did my request land" surface. Every entry reports
    /// `stored == true` by construction: the receipt gate admits nothing else,
    /// and OpenMLS empties the whole store on every merged commit, so the set is
    /// exactly what a sweep would consider.
    #[must_use]
    pub fn pending_proposals(&self) -> Vec<ProposalRecord> {
        self.inner
            .pending_proposals()
            .map(|qp| ProposalRecord {
                sender_leaf: match qp.sender() {
                    Sender::Member(idx) => Some(idx.u32()),
                    _ => None,
                },
                kind: classify(qp.proposal()),
                stored: true,
            })
            .collect()
    }

    /// This device's ADR-0045 committer rank in the group: `0` for the owner,
    /// `k+1` for the `k`-th admin, `None` for a plain member or a group with no
    /// role table. See [`crate::group_roles::committer_rank`].
    #[must_use]
    pub fn own_committer_rank(&self, identity: &Identity) -> Option<u32> {
        let table = self.group_roles()?;
        crate::group_roles::committer_rank(&table, identity.signature_public_key())
    }

    /// Sweep the authorised pending proposals into **one** Commit (ADR-0045).
    ///
    /// This is the committer half of ADR-0045: the device that has decided it is
    /// the acting committer turns everything pending into a single epoch
    /// advance, so N concurrent membership intents cost one commit rather than N
    /// racing ones. It keeps the eager-merge-then-publish shape of
    /// [`Group::add_member`] / [`Group::remove_member`] so ADR-0040's durable
    /// pending-commit outbox wraps it unchanged: the local state is at N+1 when
    /// this returns, and the caller must durably record the returned artifacts
    /// before publishing them.
    ///
    /// **A committer that can lose a race wants
    /// [`Group::stage_pending_proposals`] instead.** This method is that one
    /// followed immediately by [`Group::merge_staged_sweep`], and merging
    /// before the DS journal has ordered the commit is precisely what leaves a
    /// loser stranded at an epoch nobody else reached (G-38 residual 1). This
    /// entry point is kept, unchanged, for the callers that have not yet moved
    /// — its behaviour is byte-for-byte what it was.
    ///
    /// Returns `Ok(None)` when nothing authorised is pending — deliberately
    /// **not** an empty commit, which would advance the epoch (and race other
    /// committers) for no effect. [`ProposalCommit::swept`] is read back off the
    /// staged commit, so it reports what the commit carries rather than what the
    /// caller predicted it would. OpenMLS's own `commit_to_pending_proposals`
    /// does emit one, which is one of two reasons it is not used here; the other
    /// is that it hardcodes the proposal filter to `|_| true`.
    ///
    /// The sweep filter re-runs the ADR-0045 rule against the table as it stands
    /// now. `CommitBuilder::build` requires a filter and the one ADR-0045
    /// rejects is OpenMLS's own `|_| true`, so this is mandatory rather than
    /// belt-and-braces; it is what makes this method unable to commit an
    /// unauthorised proposal no matter which of the store's three writers put it
    /// there. Excluded proposals are dropped by the merge (OpenMLS empties the
    /// store on every merged commit), so their authors re-propose.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group has no role table, when this device's
    /// role does not authorise it to commit, or on any OpenMLS-side failure.
    pub fn commit_pending_proposals(
        &mut self,
        identity: &Identity,
    ) -> Result<Option<ProposalCommit>> {
        let staged = self.stage_pending_proposals(identity)?;
        if staged.is_some() {
            self.merge_staged_sweep(identity)?;
        }
        Ok(staged)
    }

    /// Sweep the authorised pending proposals into one Commit and **stop before
    /// merging it** — the ADR-0045 rollback point (G-38 residual 1).
    ///
    /// Identical to [`Group::commit_pending_proposals`] in what it builds and
    /// what it returns; the difference is the post-condition. This leaves the
    /// group at epoch **N** holding an un-merged staged commit, so the caller
    /// can publish, learn from the DS journal whether its commit was ordered
    /// first, and only then decide:
    ///
    /// * it won → [`Group::merge_staged_sweep`], landing at N+1;
    /// * it lost → [`Group::abandon_staged_sweep`], discarding the staged
    ///   commit and leaving the group exactly where it was, free to apply the
    ///   winner's commit and carry on.
    ///
    /// **Why this exists.** ADR-0045's designated committer makes two
    /// simultaneous commits *rare*, not impossible: no liveness timeout can
    /// tell a dead committer from a slow one, so during a handover two devices
    /// can both act. The journal already arbitrates — it is an append-only log
    /// whose order every member observes identically — so the collision needs
    /// no negotiation. What it needs is for the loser to be able to *survive
    /// losing*, which the eager-merge shape denies it: having merged N+1 before
    /// publishing, the loser sits at an epoch nobody else reached, silently and
    /// permanently. Staging instead of merging is that survival.
    ///
    /// **What the loser loses: nothing of substance.** The proposals this sweep
    /// selected are still in the pending store — OpenMLS empties that store
    /// when a commit is *merged*, and this one is not — so the winner's next
    /// sweep picks them up. Abandoning returns the group to where it was; it
    /// does not throw work away.
    ///
    /// **Relationship to ADR-0040's outbox.** The two answer different
    /// questions about the same commit and neither subsumes the other:
    ///
    /// | Question | Mechanism | Answer |
    /// |---|---|---|
    /// | Did my commit reach the wire? | the outbox | re-publish the un-acked entry |
    /// | Did my commit win the wire order? | this pair | merge, or abandon |
    ///
    /// The first must be answered *yes* before the second is even meaningful,
    /// so the sequence is: durably record → publish (or re-publish) → read the
    /// journal verdict → merge or abandon. A commit that won must be merged,
    /// never rolled back — rolling one back after peers have applied it would
    /// manufacture the very fork this closes.
    ///
    /// The staged commit is **durable**: OpenMLS persists it as part of the
    /// group state, so a crash between staging and the verdict reloads a group
    /// still at N with the sweep intact, and the restart sweep re-publishes and
    /// re-adjudicates exactly as if nothing had happened.
    ///
    /// **A staged sweep freezes the group's own membership operations** —
    /// OpenMLS refuses to build a proposal or another commit while one is
    /// pending — so a caller must reach a verdict rather than leave one staged
    /// indefinitely. Incoming messages are unaffected: applying the winner's
    /// commit works whether or not the loser has abandoned yet, because OpenMLS
    /// discards a pending commit as part of merging a foreign one. Abandoning
    /// first is the honest order and the one this crate drives; the other is
    /// pinned as a safety net so a client that gets the order wrong still
    /// converges rather than wedging.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group has no role table, when this device's
    /// role does not authorise it to commit, when a sweep is **already** staged
    /// (fail-closed: building a second one would silently replace the first,
    /// stranding a commit that may already be on the wire), or on any
    /// OpenMLS-side failure.
    pub fn stage_pending_proposals(
        &mut self,
        identity: &Identity,
    ) -> Result<Option<ProposalCommit>> {
        if self.has_staged_sweep() {
            return Err(Error::Group(
                "sweep refused: a staged sweep is already awaiting a verdict".into(),
            ));
        }
        let table = self.role_table_or_refuse("sweep")?;
        if !table.can_administer(identity.signature_public_key()) {
            return Err(Error::Group(
                "sweep refused: this device is not owner or admin".into(),
            ));
        }
        let members = self.members();

        // Would the role change this sweep is about to carry leave the group
        // with an owner who is not in it? Composing N intents into one commit is
        // exactly how that becomes reachable — "hand the group to Charlie" and
        // "remove Charlie" are each individually authorised — so the question is
        // asked once, over the whole authorised set, and its answer excludes the
        // role change rather than letting this device publish a commit every
        // peer would refuse (`owner_survives`).
        //
        // Excluding a role change changes nobody's membership, so it cannot
        // change the answer: one pass suffices, with no fixpoint to iterate.
        let role_change_survives = owner_survives(
            &commit_effects(
                self.inner
                    .pending_proposals()
                    .filter(|qp| proposal_is_authorised(&table, &members, qp)),
            ),
            &members,
        )
        .is_ok();

        // Nothing to sweep => emit no commit at all. This is a prediction of what
        // the filter below will keep, and it is used ONLY to decide emptiness;
        // `swept` is read back off the staged commit afterwards so the caller is
        // told what the commit really carried rather than what we expected it to.
        let nothing_to_sweep = !self
            .inner
            .pending_proposals()
            .any(|qp| sweep_keeps(&table, &members, role_change_survives, qp));
        if nothing_to_sweep {
            return Ok(None);
        }

        let filter_members = members;
        let filter_table = table;
        let bundle = self
            .inner
            .commit_builder()
            .consume_proposal_store(true)
            .load_psks(identity.provider.storage())
            .map_err(|e| Error::Group(format!("sweep load_psks: {e:?}")))?
            .build(
                identity.provider.rand(),
                identity.provider.crypto(),
                &identity.signature_keys,
                |qp| sweep_keeps(&filter_table, &filter_members, role_change_survives, qp),
            )
            .map_err(|e| Error::Group(format!("sweep build: {e:?}")))?
            .stage_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("sweep stage_commit: {e:?}")))?;

        // Read what the commit ACTUALLY carried off the staged commit, before
        // merging clears it. Deriving this rather than predicting it means the
        // report and the commit cannot disagree.
        let swept: Vec<ProposalRecord> = self
            .inner
            .pending_commit()
            .map(|staged| {
                staged
                    .queued_proposals()
                    .map(|qp| ProposalRecord {
                        sender_leaf: match qp.sender() {
                            Sender::Member(idx) => Some(idx.u32()),
                            _ => None,
                        },
                        kind: classify(qp.proposal()),
                        stored: true,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let (commit, welcome, _group_info) = bundle.into_messages();
        Ok(Some(ProposalCommit {
            commit,
            welcome,
            swept,
        }))
    }

    /// `true` when a sweep built by [`Group::stage_pending_proposals`] is
    /// staged and still awaiting its journal verdict.
    ///
    /// Only that method can put this group into that state. Every
    /// direct-commit path ([`Group::add_member`], [`Group::remove_member`],
    /// [`Group::set_group_context_extensions`], [`Group::self_update`],
    /// [`Group::approve_external_join`]) merges before it returns, and
    /// [`Group::commit_pending_proposals`] stages and merges in one call — so
    /// after any of them this is `false` and the verdict entry points below are
    /// no-ops. That is the whole reason a rollback here cannot reach a path
    /// that never had one.
    #[must_use]
    pub fn has_staged_sweep(&self) -> bool {
        self.inner.pending_commit().is_some()
    }

    /// **Won.** Merge the sweep staged by
    /// [`Group::stage_pending_proposals`], advancing this device to N+1 — the
    /// same state the peers who applied the published commit reached.
    ///
    /// Returns `true` when a staged sweep was merged, `false` when there was
    /// nothing staged (idempotent: a re-run after the merge is a no-op, which
    /// is what makes an ADR-0040 restart sweep safe to run twice).
    ///
    /// # Errors
    ///
    /// [`Error::Group`] on any OpenMLS-side merge failure.
    pub fn merge_staged_sweep(&mut self, identity: &Identity) -> Result<bool> {
        if !self.has_staged_sweep() {
            return Ok(false);
        }
        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;
        Ok(true)
    }

    /// **Lost.** Discard the sweep staged by
    /// [`Group::stage_pending_proposals`], returning this group to the epoch it
    /// was built against so it can apply the winner's commit and carry on.
    ///
    /// Returns `true` when a staged sweep was discarded, `false` when there was
    /// nothing staged. The `false` case is not a failure — it is what the
    /// method reports on every path that merges eagerly, and is why calling it
    /// after one cannot undo anything.
    ///
    /// **This touches one group and no other.** The rollback is
    /// `MlsGroup::clear_pending_commit`, whose entire effect is to set this
    /// group's `group_state` back to `Operational` and write that one value
    /// under this group's id. Nothing else in the provider is read or written,
    /// which is exactly what a whole-provider `Identity::restore_storage` could
    /// not promise: that restores *every* group on the account, so using it to
    /// undo one group's commit would roll sibling groups back past commits they
    /// have already published — strictly worse than the fork it was repairing.
    ///
    /// It also throws no work away: the proposals the sweep selected are still
    /// pending, because OpenMLS empties the proposal store on **merge** and
    /// this commit was never merged. The winner's sweep collects them.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] if the discard did not take effect. OpenMLS's
    /// `clear_pending_commit` is deliberately inert for a pending *external*
    /// commit, which this crate never creates — so rather than assume that,
    /// this re-reads the state afterwards and refuses to report success on a
    /// group it did not actually roll back (fail-closed, invariant #4).
    pub fn abandon_staged_sweep(&mut self, identity: &Identity) -> Result<bool> {
        if !self.has_staged_sweep() {
            return Ok(false);
        }
        self.inner
            .clear_pending_commit(identity.provider.storage())
            .map_err(|e| Error::Group(format!("clear_pending_commit: {e:?}")))?;
        if self.has_staged_sweep() {
            return Err(Error::Group(
                "abandon refused: the staged commit could not be discarded".into(),
            ));
        }
        Ok(true)
    }

    /// Apply an incoming conversation-group commit, **enforcing the ADR-0022
    /// role policy**. When the group carries a role table (every group created
    /// after ADR-0022 does, owner-bootstrap option A), a membership- or
    /// role-changing commit is merged only if the committer's role authorises
    /// it; otherwise it is **refused** — not merged — fail-closed (invariant
    /// #4). A group with no role table (legacy / pre-ADR-0022) is **open**: this
    /// behaves exactly like [`process_incoming`](Self::process_incoming).
    ///
    /// Authorisation is evaluated against the **pre-merge** role table (ADR-0022
    /// Finding 2 — a self-promoting commit is judged against the table it
    /// inherits, never the one it writes):
    ///
    /// Since [ADR-0045] the rule is applied per **proposal**, judged against the
    /// role of whoever authored it — see [`crate::group_proposals::authorise`]
    /// for the table. A commit's by-**value** proposals carry the committer as
    /// their sender, so every direct-commit path keeps behaving exactly as it
    /// did under ADR-0022; a by-**reference** proposal is judged against its
    /// original proposer, which is what stops an honest committer's sweep from
    /// blessing a plain member's injected Add.
    ///
    /// This method also carries ADR-0045's **receipt gate**: an incoming
    /// standalone proposal is retained in the pending set only if the same rule
    /// authorises its sender, and dropped otherwise, so the store this gate
    /// later sweeps can never hold anything unauthorised. A group with no role
    /// table stores nothing at all — it keeps direct commits, has no committer
    /// to elect and no gate to enforce (ADR-0045, "No migration").
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when a proposal the commit carries is not authorised for
    /// its author (the commit is left un-merged), plus the usual decode /
    /// process errors. A refused standalone *proposal* is **not** an error — it
    /// surfaces as [`Incoming::Proposal`] with `stored == false`, because
    /// dropping it already is the refusal and no group state was touched.
    ///
    /// [ADR-0045]: ../../../docs/adr/0045-remote-commit-epoch-fork.md
    pub fn process_incoming_verifying_roles(
        &mut self,
        identity: &Identity,
        bytes: &[u8],
    ) -> Result<Incoming> {
        let mls_in = decode_message(bytes)?;
        let protocol = mls_in
            .try_into_protocol_message()
            .map_err(|e| Error::Message(format!("not a protocol message: {e:?}")))?;
        let processed = self
            .inner
            .process_message(&identity.provider, protocol)
            .map_err(|e| Error::Message(format!("process_message: {e:?}")))?;

        // Capture the committer's leaf from the message envelope BEFORE
        // consuming it. A non-member (external) sender is `None`, treated as
        // unauthorised for every gated action below (fail-closed).
        let committer_leaf: Option<u32> = match processed.sender() {
            Sender::Member(idx) => Some(idx.u32()),
            _ => None,
        };

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => Ok(Incoming::Application {
                bytes: app.into_bytes(),
                sender_leaf: committer_leaf,
            }),
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                // Read the role table from the PRE-MERGE GroupContext. `None` =>
                // an open (legacy) group, no enforcement.
                if let Some(table) = self.group_roles() {
                    let members = self.members();
                    authorise_role_commit(&table, &members, &staged)?;
                }
                self.inner
                    .merge_staged_commit(&identity.provider, *staged)
                    .map_err(|e| Error::Group(format!("merge_staged_commit: {e:?}")))?;
                Ok(Incoming::Commit)
            },
            ProcessedMessageContent::ProposalMessage(qp) => {
                self.gate_incoming_proposal(identity, *qp)
            },
            ProcessedMessageContent::ExternalJoinProposalMessage(qp) => {
                self.retain_external_join(&qp)
            },
        }
    }

    /// ADR-0045 **receipt gate** — retain an incoming standalone proposal only
    /// when the role table authorises its sender, and drop it otherwise.
    ///
    /// Gating here, rather than only filtering at sweep time, does more than
    /// keep the store clean. A Commit carries a member proposal **by
    /// reference**, and OpenMLS resolves that reference against the receiver's
    /// own store — so a peer that dropped a proposal here cannot apply a commit
    /// that carries it either, failing with `InvalidCommit(MissingProposal)`
    /// before any Lakko gate runs. Dropping at receipt is therefore a boundary
    /// in its own right, not merely hygiene. (It also has to be a drop rather
    /// than a later eviction: `ProposalStore::remove` is `pub(crate)`, so
    /// anything admitted can only be cleared wholesale.)
    ///
    /// A group with **no role table** stores nothing — see
    /// [`Self::process_incoming_verifying_roles`].
    fn gate_incoming_proposal(
        &mut self,
        identity: &Identity,
        qp: QueuedProposal,
    ) -> Result<Incoming> {
        let sender_leaf = match qp.sender() {
            Sender::Member(idx) => Some(idx.u32()),
            _ => None,
        };
        let kind = classify(qp.proposal());
        let refused = |kind: ProposalKind| {
            Ok(Incoming::Proposal(ProposalRecord {
                sender_leaf,
                kind,
                stored: false,
            }))
        };

        let Some(table) = self.group_roles() else {
            return refused(kind);
        };
        let members = self.members();
        let proposer = sender_leaf.and_then(|leaf| {
            pubkey_at_leaf(&members, leaf).map(|pubkey| Proposer { leaf, pubkey })
        });
        if authorise(&table, &members, proposer, &kind).is_err() {
            return refused(kind);
        }

        // Already held. A `ProposalRef` is a hash over the proposal's
        // authenticated content, so N fresh encryptions of one plaintext
        // proposal all decrypt to the same ref — the ratchet refuses only a
        // byte-identical replay, and `ProposalStore::add` is a bare push with
        // no de-duplication of its own. Without this a plain member could file
        // its own (correctly authorised) departure hundreds of times and make
        // every member carry, persist and re-park all of them. Reported as
        // `stored: true`, because it is: the group holds this exact request,
        // and the proposer's "did it land" answer is yes.
        if self
            .inner
            .pending_proposals()
            .any(|held| held.proposal_reference_ref() == qp.proposal_reference_ref())
        {
            return Ok(Incoming::Proposal(ProposalRecord {
                sender_leaf,
                kind,
                stored: true,
            }));
        }

        // Full. Refuse the newcomer rather than evict an incumbent — see
        // [`MAX_PENDING_PROPOSALS`] for why that direction is the only safe one.
        if self.inner.pending_proposals().count() >= MAX_PENDING_PROPOSALS {
            return refused(kind);
        }

        self.inner
            .store_pending_proposal(identity.provider.storage(), qp)
            .map_err(|e| Error::Group(format!("store_pending_proposal: {e:?}")))?;
        Ok(Incoming::Proposal(ProposalRecord {
            sender_leaf,
            kind,
            stored: true,
        }))
    }

    /// Encrypt an application payload for the current group epoch.
    ///
    /// **A pending membership proposal must never mute the group.** OpenMLS
    /// refuses `MlsGroup::create_message` outright while its proposal store is
    /// non-empty (`openmls-0.8.1/src/group/mls_group/application.rs:27-31`,
    /// `MlsGroupStateError::PendingProposal`) — its assumption being that a
    /// caller commits or clears the store promptly. ADR-0045 breaks that
    /// assumption on purpose: an authorised proposal is retained until a
    /// *designated committer* sweeps it, and no liveness rule can distinguish a
    /// dead committer from a slow one, so a request may legitimately wait hours.
    /// Left alone, that would take the whole group's **chat** down from the
    /// moment one admin proposes — a consequence ADR-0045 did not record and
    /// which is not acceptable in a messenger.
    ///
    /// So this method **parks** the pending set for the duration of the encrypt
    /// call: it lifts the proposals out, sends, and puts them back exactly as
    /// they were — same proposals, same order, same [`ProposalRef`]s (the ref is
    /// derived from the proposal, so a round-trip through
    /// `store_pending_proposal` reproduces it bit for bit). Everything else
    /// about the proposal path is untouched.
    ///
    /// # The park is in-memory only, and that is load-bearing
    ///
    /// The check OpenMLS makes is `self.proposal_store().is_empty()` — a read
    /// of the **in-memory** `ProposalStore` and nothing else. Persistence has
    /// no say in it. The two entry points that mutate that store,
    /// `clear_pending_proposals` and `store_pending_proposal`, are each generic
    /// over the storage they are handed and each does two things: mutate the
    /// in-memory store, and mirror that mutation into that storage. Handing
    /// them a **throwaway** `MemoryStorage` takes the first and discards the
    /// second, so the whole park happens without one write to the real
    /// provider.
    ///
    /// **That is what keeps a concurrent snapshot whole**, and the earlier
    /// reasoning here — that `&mut self` on the group excludes one — was
    /// wrong. [`crate::Identity::snapshot_storage`] takes `&self` on the
    /// *identity*, a different object, and reads the provider's map under its
    /// own `RwLock`; nothing about an exclusive borrow of the `Group` prevents
    /// it. On iOS and Android that is not hypothetical — the UniFFI `Group` is
    /// `Mutex<MlsGroup>` while `Identity` is bare, so the send thread and the
    /// persist thread genuinely run at once. A park that emptied the persisted
    /// queue for the length of an encrypt therefore let a snapshot capture a
    /// group with **no** pending proposals, and a client that persisted that
    /// snapshot and restarted could never apply the by-reference commit those
    /// proposals belong to (`InvalidCommit(MissingProposal)`) — silently,
    /// permanently, and needing no adversary. Since the real queue is now never
    /// written, a snapshot taken at any instant during a send observes the set
    /// exactly as it stood before it, which is also exactly how it stands
    /// after.
    ///
    /// # What is *not* changed, and why
    ///
    /// * **Where a pending proposal lives.** It stays in the OpenMLS storage
    ///   provider's proposal queue — the same place `store_pending_proposal`
    ///   has always put it, the place [`crate::Identity::snapshot_storage`]
    ///   serialises and `MlsGroup::load` rebuilds from. This crate invents no
    ///   persistence of its own, and a restart keeps the pending set exactly as
    ///   it did before.
    /// * **By-reference resolution.** A Commit carries a member proposal by
    ///   reference and OpenMLS resolves it against the **receiver's** store
    ///   (`public_group/staged_commit.rs`, `ProposalQueue::from_committed_proposals`
    ///   → `StageCommitError::MissingProposal`). That resolution happens on
    ///   every member at commit receipt, not only on the committer at sweep
    ///   time, so the store has to be populated on every member for the whole
    ///   interval between the proposal and the commit. Parking only across the
    ///   encrypt call is what keeps that true — the store is whole on entry and
    ///   whole on exit, and no message is processed in between.
    /// * **The authorisation gate.** Untouched, and still at receipt: nothing
    ///   unauthorised is ever parked, because nothing unauthorised is ever
    ///   stored.
    ///
    /// # Failure handling
    ///
    /// The unpark runs before either outcome is propagated, and a failed unpark
    /// wins over a successful send: silently dropping a pending proposal would
    /// desync this device from every peer that still holds it (their commit
    /// would later fail to resolve), whereas a refused send costs one message
    /// the caller can retry. A produced-then-discarded ciphertext is safe — it
    /// is indistinguishable from a message lost in transit, which MLS receivers
    /// already tolerate.
    ///
    /// The unpark restores **every** proposal before reporting any failure, and
    /// keeps only the first error. Stopping at the first would leave the
    /// in-memory store holding a torn prefix of the set while the persisted
    /// queue still held all of it — a divergence a reload would silently
    /// repair, and until then one this device would sweep from.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Message`] on encryption failure, or [`Error::Group`] if
    /// the pending set could not be parked or restored.
    pub fn create_application_message(
        &mut self,
        identity: &Identity,
        plaintext: &[u8],
    ) -> Result<MlsMessageOut> {
        // Nothing pending — the overwhelmingly common case, and byte-for-byte
        // the pre-ADR-0045 path with no proposal-store traffic at all.
        let parked: Vec<QueuedProposal> = self.inner.pending_proposals().cloned().collect();
        if parked.is_empty() {
            return self.encrypt_application(identity, plaintext);
        }

        // The throwaway that makes the park invisible to persistence. Both
        // mutators below write it instead of `identity.provider`, so the real
        // provider's proposal queue is not touched at any point in this method
        // and no concurrent `snapshot_storage` can observe a torn set.
        let scratch = MemoryStorage::default();

        self.inner
            .clear_pending_proposals(&scratch)
            .map_err(|e| Error::Group(format!("park pending proposals: {e:?}")))?;
        let sent = self.encrypt_application(identity, plaintext);

        let mut unparked: Option<Error> = None;
        for qp in parked {
            if let Err(e) = self.inner.store_pending_proposal(&scratch, qp) {
                if unparked.is_none() {
                    unparked = Some(Error::Group(format!("restore pending proposals: {e:?}")));
                }
            }
        }
        if let Some(e) = unparked {
            return Err(e);
        }
        sent
    }

    /// The bare OpenMLS encrypt, with no proposal-store handling — the single
    /// place [`Group::create_application_message`]'s two paths meet, so the
    /// parked path and the fast path cannot drift.
    fn encrypt_application(
        &mut self,
        identity: &Identity,
        plaintext: &[u8],
    ) -> Result<MlsMessageOut> {
        self.inner
            .create_message(&identity.provider, &identity.signature_keys, plaintext)
            .map_err(|e| Error::Message(format!("create_message: {e:?}")))
    }

    /// Decrypt and advance the state machine for an incoming MLS message.
    ///
    /// `bytes` is the TLS-encoded [`MlsMessageOut`] from the wire. The
    /// returned [`Incoming`] tells the caller what kind of payload arrived
    /// (application, commit, proposal).
    ///
    /// Commits are auto-merged into the local group state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Codec`] for malformed wire data, [`Error::Message`]
    /// for processing failures, or [`Error::Unsupported`] for message kinds
    /// we don't yet handle.
    pub fn process_incoming(&mut self, identity: &Identity, bytes: &[u8]) -> Result<Incoming> {
        let mls_in = decode_message(bytes)?;
        let protocol = mls_in
            .try_into_protocol_message()
            .map_err(|e| Error::Message(format!("not a protocol message: {e:?}")))?;

        let processed = self
            .inner
            .process_message(&identity.provider, protocol)
            .map_err(|e| Error::Message(format!("process_message: {e:?}")))?;

        // Capture the sender's leaf before consuming the message (ADR-0023 —
        // the chat receive path needs it for the author-only edit/delete gate).
        let sender_leaf: Option<u32> = match processed.sender() {
            Sender::Member(idx) => Some(idx.u32()),
            _ => None,
        };

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => Ok(Incoming::Application {
                bytes: app.into_bytes(),
                sender_leaf,
            }),
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                self.inner
                    .merge_staged_commit(&identity.provider, *staged)
                    .map_err(|e| Error::Group(format!("merge_staged_commit: {e:?}")))?;
                Ok(Incoming::Commit)
            },
            // ADR-0045: this is the OPEN path (DeviceGroups, call groups, and
            // any group with no role table). It has no role table to judge a
            // proposer against, so it deliberately stores NOTHING — surfacing
            // the proposal as data while leaving the pending set untouched.
            // Storing here would be exactly the ungated store ADR-0045 exists
            // to prevent.
            ProcessedMessageContent::ProposalMessage(qp) => {
                Ok(Incoming::Proposal(ProposalRecord {
                    sender_leaf,
                    kind: classify(qp.proposal()),
                    stored: false,
                }))
            },
            ProcessedMessageContent::ExternalJoinProposalMessage(qp) => {
                self.retain_external_join(&qp)
            },
        }
    }

    /// Like [`Self::process_incoming`], but for a Commit the `gate` is invoked
    /// with the set of `(removed_leaf_index, that leaf's CURRENT signature
    /// public key)` pairs **before** the commit is merged. If `gate` returns
    /// `Err`, the commit is NOT merged and the error propagates — the
    /// refuse-to-apply hook for peer-verified device-removal authorisation
    /// (G-13, [ADR-0015]). Non-commit messages bypass the gate entirely.
    ///
    /// The removed leaves' signature keys are read from the pre-merge tree
    /// (the leaves still exist), so the caller can recompute what a removal
    /// attestation must bind to.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::process_incoming`]'s errors, plus whatever `gate`
    /// returns on a refused removal.
    ///
    /// [ADR-0015]: ../../../docs/adr/0015-device-removal-authorization.md
    pub fn process_incoming_gated<F>(
        &mut self,
        identity: &Identity,
        bytes: &[u8],
        gate: F,
    ) -> Result<Incoming>
    where
        F: FnOnce(&[(LeafNodeIndex, Vec<u8>)]) -> Result<()>,
    {
        let mls_in = decode_message(bytes)?;
        let protocol = mls_in
            .try_into_protocol_message()
            .map_err(|e| Error::Message(format!("not a protocol message: {e:?}")))?;

        let processed = self
            .inner
            .process_message(&identity.provider, protocol)
            .map_err(|e| Error::Message(format!("process_message: {e:?}")))?;

        let sender_leaf: Option<u32> = match processed.sender() {
            Sender::Member(idx) => Some(idx.u32()),
            _ => None,
        };

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => Ok(Incoming::Application {
                bytes: app.into_bytes(),
                sender_leaf,
            }),
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                // Read removed leaves + their pre-merge signature keys.
                let members = self.members();
                let removed: Vec<(LeafNodeIndex, Vec<u8>)> = staged
                    .remove_proposals()
                    .map(|qrp| qrp.remove_proposal().removed())
                    .map(|leaf| {
                        let pk = members
                            .iter()
                            .find(|m| m.leaf_index == leaf.u32())
                            .map(|m| m.signature_public_key.clone())
                            .unwrap_or_default();
                        (leaf, pk)
                    })
                    .collect();

                // Refuse-to-apply hook — runs before the merge.
                gate(&removed)?;

                self.inner
                    .merge_staged_commit(&identity.provider, *staged)
                    .map_err(|e| Error::Group(format!("merge_staged_commit: {e:?}")))?;
                Ok(Incoming::Commit)
            },
            // ADR-0045: this is the OPEN path (DeviceGroups, call groups, and
            // any group with no role table). It has no role table to judge a
            // proposer against, so it deliberately stores NOTHING — surfacing
            // the proposal as data while leaving the pending set untouched.
            // Storing here would be exactly the ungated store ADR-0045 exists
            // to prevent.
            ProcessedMessageContent::ProposalMessage(qp) => {
                Ok(Incoming::Proposal(ProposalRecord {
                    sender_leaf,
                    kind: classify(qp.proposal()),
                    stored: false,
                }))
            },
            ProcessedMessageContent::ExternalJoinProposalMessage(qp) => {
                self.retain_external_join(&qp)
            },
        }
    }

    /// Derive a `length`-byte secret bound to this group's current epoch.
    ///
    /// Wraps `MlsGroup::export_secret` (RFC 9420 §8.5). The output is the
    /// canonical way to bridge MLS to non-MLS protocols (SFrame media keys,
    /// channel content keys, etc.). The result is wrapped in [`Zeroizing`]
    /// so it is wiped on drop.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Exporter`] on internal failure (e.g. zero length).
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: &str,
        context: &[u8],
        length: usize,
    ) -> Result<Zeroizing<Vec<u8>>> {
        let secret = self
            .inner
            .export_secret(identity.provider.crypto(), label, context, length)
            .map_err(|e| Error::Exporter(format!("{e:?}")))?;
        Ok(Zeroizing::new(secret))
    }

    /// Local leaf index of `identity` in the group, if present.
    #[must_use]
    pub fn own_leaf_index(&self) -> LeafNodeIndex {
        self.inner.own_leaf_index()
    }

    /// Current group epoch (advances on every Commit).
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch().as_u64()
    }

    /// Number of members in the group at the current epoch.
    #[must_use]
    pub fn member_count(&self) -> usize {
        self.inner.members().count()
    }

    /// Snapshot of every member's `(leaf_index, signature_public_key)`
    /// at the current epoch (Phase 1.18h).
    ///
    /// Returns a `Vec` rather than an iterator so the wasm-bindgen
    /// surface doesn't have to thread a streaming protocol —
    /// member sets are bounded by the application (≤ 1k for
    /// Phase 1 groups per the ROADMAP) and a one-shot allocation
    /// is cheaper than the cross-FFI dance.
    ///
    /// Ordering follows OpenMLS's internal traversal (left-to-right
    /// over the leaf nodes), which is stable across calls inside a
    /// single epoch and recomputed on every `process_incoming`
    /// commit. Callers MUST NOT cache the index for use across
    /// epochs — a remove/add cycle can renumber leaves.
    #[must_use]
    pub fn members(&self) -> Vec<MemberRecord> {
        self.inner
            .members()
            .map(|m| MemberRecord {
                leaf_index: m.index.u32(),
                signature_public_key: m.signature_key,
            })
            .collect()
    }

    /// MLS `group_id` (RFC 9420 §6.2) — the opaque identifier
    /// shared by every member of the group at every epoch.
    ///
    /// Used as the routing key for the DS group journal at
    /// `POST /ds/group/{group_id_hex}/messages`. Length is
    /// implementation-defined by OpenMLS (typically 32 bytes
    /// of randomness chosen at `Group::create`); callers
    /// should treat it as opaque bytes.
    #[must_use]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.group_id().as_slice().to_vec()
    }

    /// Reconstruct a `Group` from a previously persisted MLS
    /// state in `identity.provider.storage()`. Pair with
    /// [`Identity::restore_storage`] to load both halves of
    /// the snapshot back into a single live `Identity` +
    /// `Group` pair (Phase 1.16e6a).
    ///
    /// Returns:
    ///
    /// * `Ok(Some(group))` — the storage contains a group
    ///   under `group_id`; the loaded ratchet state advances
    ///   from there as if the process never restarted.
    /// * `Ok(None)` — no group with that id is in storage
    ///   (typical case: the snapshot was wiped, or the caller
    ///   is asking for a different group than was persisted).
    /// * `Err(Error::Group)` — the storage row exists but
    ///   OpenMLS rejected its contents (cipher suite mismatch,
    ///   schema drift on storage upgrade, …).
    ///
    /// # Errors
    ///
    /// Any `MlsGroup::load` failure surfaces as
    /// [`Error::Group`] with the underlying OpenMLS message
    /// preserved.
    pub fn load(identity: &Identity, group_id: &[u8]) -> Result<Option<Self>> {
        use openmls::group::GroupId;
        let id = GroupId::from_slice(group_id);
        let loaded = MlsGroup::load(identity.provider.storage(), &id)
            .map_err(|e| Error::Group(format!("load: {e:?}")))?;
        Ok(loaded.map(Self::from_mls_group))
    }

    /// Wrap an already-constructed [`MlsGroup`] in the Lakko
    /// [`Group`] shape. Used by [`crate::device_group::DeviceGroup`]
    /// (Phase 2a3a) to share the public surface without duplicating
    /// every method. Not part of the supported public API — callers
    /// outside the crate should go through [`Group::create`] or
    /// [`Group::join_from_welcome`].
    #[doc(hidden)]
    pub(crate) fn from_mls_group(inner: MlsGroup) -> Self {
        Self {
            inner,
            pending_external_joins: Vec::new(),
        }
    }
}

/// The conversation-group `required_capabilities` extension declaring the
/// ADR-0022 role-table type (`0xFF02`), so a `GroupContextExtensions` commit
/// may carry / rewrite the table (OpenMLS refuses to add an extension whose
/// type is not in `required_capabilities`).
fn role_required_capabilities() -> Extension {
    Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
        &[ExtensionType::Unknown(ROLE_EXTENSION_TYPE)],
        &[],
        &[],
    ))
}

/// `true` when `qp` is authorised for the member that **authored** it, judged
/// against `table` as it stands now.
///
/// Shared by the ADR-0045 sweep's emptiness prediction and its filter, so the
/// two cannot answer the question differently — a disagreement there would mean
/// emitting a commit that carries nothing, or skipping one that would have
/// carried something.
fn proposal_is_authorised(
    table: &RoleTable,
    members: &[MemberRecord],
    qp: &QueuedProposal,
) -> bool {
    let leaf = match qp.sender() {
        Sender::Member(idx) => Some(idx.u32()),
        _ => None,
    };
    let proposer =
        leaf.and_then(|leaf| pubkey_at_leaf(members, leaf).map(|pubkey| Proposer { leaf, pubkey }));
    authorise(table, members, proposer, &classify(qp.proposal())).is_ok()
}

/// `true` when the ADR-0045 sweep should carry `qp`: authorised for its author,
/// and — for a role change — not one that would leave the group with an owner
/// outside it (`role_change_survives`, computed once over the whole authorised
/// set by [`Group::commit_pending_proposals`]).
///
/// The second clause is an **exclusion, not a refusal**: a role change dropped
/// here is simply not committed, its author re-proposes, and the rest of the
/// sweep still lands. Refusing the whole sweep instead would let one bad role
/// change block every unrelated membership change in the group.
fn sweep_keeps(
    table: &RoleTable,
    members: &[MemberRecord],
    role_change_survives: bool,
    qp: &QueuedProposal,
) -> bool {
    proposal_is_authorised(table, members, qp)
        && (role_change_survives || !matches!(classify(qp.proposal()), ProposalKind::RoleChange))
}

/// Enforce the ADR-0022 role policy on a staged commit BEFORE it is merged,
/// against the **pre-merge** `table`. Returns `Err` (the commit must NOT be
/// merged) when any proposal the commit carries is not authorised for its
/// author.
///
/// **This is the ADR-0045 security boundary.** The rule is applied per proposal
/// rather than once per commit, because ADR-0045 lets a member's proposal be
/// swept by someone else's commit: judging only the committer would let a plain
/// member inject an `Add` that an honest owner's sweep then blesses. Each
/// `QueuedProposal` is judged against **its own sender**, which is:
///
/// * for a by-**reference** proposal, the original proposer — the new check;
/// * for a by-**value** proposal, the committer itself — byte-for-byte the
///   pre-ADR-0045 behaviour, so every direct-commit path is unchanged.
///
/// The rule itself lives once, in [`crate::group_proposals::authorise`]. A
/// commit carrying no proposals at all (a bare `self_update` path) is
/// authorised, as it was before.
///
/// One further rule is applied to the commit **as a whole** rather than to any
/// one proposal: [`crate::group_proposals::owner_survives`] refuses a commit
/// that would leave the group with an owner who is not a member of it. It
/// cannot be a per-proposal rule, because the two ways to reach that state are a
/// role table naming a key no leaf holds (one proposal, but the roster is not
/// visible to `authorise`) and a transfer riding alongside the new owner's
/// removal (two individually-authorised proposals). It runs FIRST, so a commit
/// that would brick the group is refused before anything else is considered.
///
/// Note on "leaving": OpenMLS `CannotRemoveSelf` forbids a member from
/// *committing* the removal of its own leaf, so a committer is never inside its
/// own commit's removed set. ADR-0045 makes leaving reachable the other way —
/// the member *proposes* it and an admin commits — which the shared rule
/// handles for both `SelfRemove` and a plain `Remove` naming the proposer's own
/// leaf.
fn authorise_role_commit(
    table: &RoleTable,
    members: &[MemberRecord],
    staged: &StagedCommit,
) -> Result<()> {
    // The one rule that is about the commit rather than about any one proposal:
    // whatever else this commit does, it must not leave the group with an owner
    // who is not in it. See `owner_survives` for the two ways that happens and
    // why neither is visible to a per-proposal check.
    owner_survives(&commit_effects(staged.queued_proposals()), members)?;
    for qp in staged.queued_proposals() {
        // The proposal's own author: the sender OpenMLS recorded for it. A
        // by-value proposal carries the committer; a by-reference one carries
        // whoever broadcast it. Anything that is not a resolvable member leaf
        // (an external sender, a leaf not in the pre-merge roster) yields no
        // `Proposer`, and `authorise` refuses every kind in that case.
        let author_leaf = match qp.sender() {
            Sender::Member(idx) => Some(idx.u32()),
            // An external sender (`NewMemberCommit` on an external commit,
            // `External`, `NewMemberProposal`) resolves to no member leaf, so
            // `authorise` refuses — which is what the pre-ADR-0045 gate did
            // with an unresolvable committer too.
            _ => None,
        };
        let author = author_leaf
            .and_then(|leaf| pubkey_at_leaf(members, leaf).map(|pubkey| Proposer { leaf, pubkey }));
        authorise(table, members, author, &classify(qp.proposal()))?;
    }
    Ok(())
}

/// Encode any [`MlsMessageOut`] to its TLS wire form.
///
/// # Errors
///
/// Returns [`Error::Codec`] on encoder failure.
pub fn encode_message(msg: &MlsMessageOut) -> Result<Vec<u8>> {
    msg.tls_serialize_detached()
        .map_err(|e| Error::Codec(format!("{e:?}")))
}

/// Decode a wire-form MLS message into an [`MlsMessageIn`].
///
/// # Errors
///
/// Returns [`Error::Codec`] on parse failure.
fn decode_message(bytes: &[u8]) -> Result<MlsMessageIn> {
    let mut slice = bytes;
    MlsMessageIn::tls_deserialize(&mut slice).map_err(|e| Error::Codec(format!("{e:?}")))
}

/// Read the MLS **epoch** out of a wire-form message's public framing, without
/// any group secret (ADR-0028 §3 — the join-ticket joiner side).
///
/// Both `PrivateMessage` and `PublicMessage` carry `group_id` + `epoch` in
/// cleartext ahead of any encrypted content, so a **non-member** polling a
/// group's journal can read the group's current epoch from its latest message
/// to craft an external-join proposal for the right epoch
/// ([`Identity::create_external_join_proposal`](crate::Identity::create_external_join_proposal)).
///
/// Returns `None` for bytes that do not parse as an `MlsMessageIn`, or for a
/// message variant with no epoch (a `Welcome` / `KeyPackage` / `GroupInfo`) —
/// the caller treats an empty / epoch-less journal head as epoch `0` (a
/// freshly created group with no commits yet).
#[must_use]
pub fn epoch_of_message(bytes: &[u8]) -> Option<u64> {
    let mut slice = bytes;
    let msg = MlsMessageIn::tls_deserialize(&mut slice).ok()?;
    let proto = msg.try_into_protocol_message().ok()?;
    Some(proto.epoch().as_u64())
}

// ============================================================================
// ADR-0045 — the adversary half of the proposal gate's property tests.
//
// These live in the lib (not `tests/`) because a genuine test of the receipt
// gate and the sweep filter needs an adversary that SKIPS them, and the crate
// deliberately publishes no ungated store / commit-all entry point for one to
// use. The `#[cfg(test)]` helpers below are that adversary: they are exactly
// what a modified client would do — `propose_*` without the local check,
// `store_pending_proposal` without the receipt gate, and `CommitBuilder::build`
// with OpenMLS's own `|_| true` filter, which is what
// `commit_to_pending_proposals` hardcodes.
//
// The structural reason a peer can never meet an unauthorised proposal from an
// HONEST client is worth stating: the ADR-0022 role table lives in the
// authenticated GroupContext, so two devices at the same epoch necessarily
// agree on every role, and a proposal framed at any other epoch is refused by
// ValSem003 before a gate is consulted. An unauthorised proposal at a matching
// epoch therefore always means a modified client — which is exactly the
// adversary modelled here.
// ============================================================================

#[cfg(test)]
impl Group {
    /// **Adversary.** Propose an Add with no local authorisation check.
    fn ungated_propose_add(&mut self, identity: &Identity, kp: KeyPackage) -> Result<Vec<u8>> {
        let (proposal, _r) = self
            .inner
            .propose_add_member(&identity.provider, &identity.signature_keys, &kp)
            .map_err(|e| Error::Group(format!("ungated_propose_add: {e:?}")))?;
        encode_message(&proposal)
    }

    /// **Adversary.** Store an incoming proposal with no receipt gate.
    fn ungated_store(&mut self, identity: &Identity, bytes: &[u8]) -> Result<()> {
        let mls_in = decode_message(bytes)?;
        let protocol = mls_in
            .try_into_protocol_message()
            .map_err(|e| Error::Message(format!("not a protocol message: {e:?}")))?;
        let processed = self
            .inner
            .process_message(&identity.provider, protocol)
            .map_err(|e| Error::Message(format!("process_message: {e:?}")))?;
        let ProcessedMessageContent::ProposalMessage(qp) = processed.into_content() else {
            return Err(Error::Group("not a proposal".into()));
        };
        self.inner
            .store_pending_proposal(identity.provider.storage(), *qp)
            .map_err(|e| Error::Group(format!("ungated_store: {e:?}")))
    }

    /// **Adversary.** Remove a member the way [`Group::remove_member`] did
    /// before the ADR-0045 review's HIGH-1 guard: straight into OpenMLS's
    /// `remove_members`, which builds with `consume_proposal_store` at its
    /// default `true` and the filter hardcoded to `|_| true`, and merges
    /// eagerly. Exists so a control test can reproduce the fork the guard
    /// prevents, rather than the suite merely asserting that it does not
    /// happen.
    fn ungated_remove_member(
        &mut self,
        identity: &Identity,
        leaf: LeafNodeIndex,
    ) -> Result<MlsMessageOut> {
        let (commit, _welcome, _group_info) = self
            .inner
            .remove_members(&identity.provider, &identity.signature_keys, &[leaf])
            .map_err(|e| Error::Group(format!("ungated_remove_member: {e:?}")))?;
        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;
        Ok(commit)
    }

    /// **Adversary.** Sweep the whole pending store with no filter — OpenMLS's
    /// `commit_to_pending_proposals` behaviour, which ADR-0045 rejects.
    fn ungated_sweep(&mut self, identity: &Identity) -> Result<MlsMessageOut> {
        let bundle = self
            .inner
            .commit_builder()
            .consume_proposal_store(true)
            .load_psks(identity.provider.storage())
            .map_err(|e| Error::Group(format!("{e:?}")))?
            .build(
                identity.provider.rand(),
                identity.provider.crypto(),
                &identity.signature_keys,
                |_| true,
            )
            .map_err(|e| Error::Group(format!("{e:?}")))?
            .stage_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("{e:?}")))?;
        let commit = bundle.into_commit();
        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;
        Ok(commit)
    }
}

#[cfg(test)]
mod proposal_gate_tests {
    use super::*;
    use crate::group_roles::RoleTable;
    use crate::key_package;

    fn wire(m: MlsMessageOut) -> Vec<u8> {
        encode_message(&m).expect("encode")
    }

    fn kp(id: &Identity) -> KeyPackage {
        let bytes =
            key_package::serialize(&id.create_key_package().expect("kp")).expect("serialize");
        key_package::deserialize(&bytes)
            .expect("deserialize")
            .validate(id.provider.crypto(), ProtocolVersion::Mls10)
            .expect("validate")
    }

    fn kp_wire(id: &Identity) -> Vec<u8> {
        key_package::serialize(&id.create_key_package().expect("kp")).expect("serialize kp")
    }

    fn pk32(id: &Identity) -> [u8; 32] {
        id.signature_public_key().try_into().expect("32-byte key")
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
        let alice = Identity::from_seed(&[0x40 ^ tag; 64]).expect("alice");
        let bob = Identity::from_seed(&[0x50 ^ tag; 64]).expect("bob");
        let charlie = Identity::from_seed(&[0x60 ^ tag; 64]).expect("charlie");

        let mut alice_g = Group::create(&alice).expect("create");
        let add_bob = alice_g
            .add_member_from_wire(&alice, &kp_wire(&bob))
            .expect("add bob");
        let mut bob_g = Group::join_from_welcome(&wire(add_bob.welcome), &bob).expect("bob joins");
        let add_c = alice_g
            .add_member_from_wire(&alice, &kp_wire(&charlie))
            .expect("add charlie");
        bob_g
            .process_incoming_verifying_roles(&bob, &wire(add_c.commit))
            .expect("bob applies");
        let mut charlie_g =
            Group::join_from_welcome(&wire(add_c.welcome), &charlie).expect("charlie joins");
        let promote = alice_g
            .set_group_roles(&alice, &RoleTable::new(pk32(&alice), vec![pk32(&bob)]))
            .expect("promote bob");
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

    /// Layer 1. A modified client's proposal reaches a peer at a matching
    /// epoch; the receipt gate must drop it, unstored.
    #[test]
    fn receipt_gate_drops_a_plain_members_proposal() {
        let mut s = scene(0x01);
        let dave = Identity::from_seed(&[0x71u8; 64]).expect("dave");
        let dave_key = dave.signature_public_key().to_vec();

        let hostile = s
            .charlie_g
            .ungated_propose_add(&s.charlie, kp(&dave))
            .expect("charlie proposes without its local check");

        let incoming = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, &hostile)
            .expect("alice processes it");
        let Incoming::Proposal(record) = incoming else {
            panic!("expected a proposal, got {incoming:?}");
        };
        assert!(
            !record.stored,
            "a plain member's Add proposal must NOT be stored",
        );
        assert_eq!(
            record.kind,
            ProposalKind::Add {
                signature_key: dave_key
            },
            "the refusal must still describe what was refused",
        );
        assert!(s.alice_g.pending_proposals().is_empty());
    }

    /// Layer 2, in isolation. A proposal the store already holds but the table
    /// does not authorise must be excluded by the sweep filter — proven by
    /// planting it with the ungated store and then running the REAL sweep.
    #[test]
    fn sweep_filter_excludes_an_unauthorised_proposal_the_store_already_holds() {
        let mut s = scene(0x02);
        let dave = Identity::from_seed(&[0x72u8; 64]).expect("dave");

        let hostile = s
            .charlie_g
            .ungated_propose_add(&s.charlie, kp(&dave))
            .expect("charlie proposes ungated");
        s.alice_g
            .ungated_store(&s.alice, &hostile)
            .expect("alice stores it ungated");
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            1,
            "the store is primed with an unauthorised proposal",
        );

        // The real sweep finds nothing it may commit, so it emits no commit.
        assert!(
            s.alice_g
                .commit_pending_proposals(&s.alice)
                .expect("sweep")
                .is_none(),
            "the sweep filter must exclude the unauthorised proposal",
        );
        assert!(!s
            .alice_g
            .members()
            .iter()
            .any(|m| m.signature_public_key == dave.signature_public_key()));
    }

    /// Layer 3, in isolation — the peer-verified commit gate. A malicious
    /// committer skips layers 1 and 2 entirely; a peer that ALSO happens to
    /// hold the proposal (so OpenMLS can resolve the by-reference commit) must
    /// still refuse to merge.
    #[test]
    fn peer_refuses_a_commit_carrying_a_proposal_from_an_unauthorised_sender() {
        let mut s = scene(0x03);
        let dave = Identity::from_seed(&[0x73u8; 64]).expect("dave");

        let hostile = s
            .charlie_g
            .ungated_propose_add(&s.charlie, kp(&dave))
            .expect("charlie proposes ungated");

        // bob (admin) is the malicious committer: stores ungated, sweeps
        // unfiltered — exactly `commit_to_pending_proposals`.
        s.bob_g
            .ungated_store(&s.bob, &hostile)
            .expect("bob stores ungated");
        let commit = s
            .bob_g
            .ungated_sweep(&s.bob)
            .expect("bob sweeps unfiltered");

        // alice ALSO holds the proposal (planted the same way), so the
        // by-reference lookup succeeds and the commit gate is what decides.
        s.alice_g
            .ungated_store(&s.alice, &hostile)
            .expect("alice stores ungated");
        let epoch_before = s.alice_g.epoch();
        assert!(
            s.alice_g
                .process_incoming_verifying_roles(&s.alice, &wire(commit))
                .is_err(),
            "the commit gate must refuse a commit carrying a plain member's Add",
        );
        assert_eq!(
            s.alice_g.epoch(),
            epoch_before,
            "a refused commit must NOT advance the epoch",
        );
        assert!(!s
            .alice_g
            .members()
            .iter()
            .any(|m| m.signature_public_key == dave.signature_public_key()));
    }

    /// Layers 1 and 3 composed, which is the deployed shape: a peer whose
    /// receipt gate dropped the proposal cannot even resolve the by-reference
    /// commit, so OpenMLS refuses it before the gate is reached. Fail-closed
    /// twice over, and the reason the receipt gate is not merely hygiene.
    #[test]
    fn a_peer_that_dropped_the_proposal_cannot_apply_a_commit_referencing_it() {
        let mut s = scene(0x04);
        let dave = Identity::from_seed(&[0x74u8; 64]).expect("dave");

        let hostile = s
            .charlie_g
            .ungated_propose_add(&s.charlie, kp(&dave))
            .expect("charlie proposes ungated");
        s.bob_g
            .ungated_store(&s.bob, &hostile)
            .expect("bob stores ungated");
        let commit = s
            .bob_g
            .ungated_sweep(&s.bob)
            .expect("bob sweeps unfiltered");

        // alice ran the real receipt gate, so it never stored the proposal.
        let incoming = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, &hostile)
            .expect("alice gates it");
        assert!(matches!(incoming, Incoming::Proposal(ref r) if !r.stored));

        let epoch_before = s.alice_g.epoch();
        let err = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, &wire(commit))
            .expect_err("the commit must be refused");
        assert!(
            format!("{err:?}").contains("MissingProposal"),
            "expected an unresolvable by-reference proposal, got {err:?}",
        );
        assert_eq!(s.alice_g.epoch(), epoch_before);
    }

    /// Leaving, and the precondition that bounds it.
    ///
    /// ADR-0045 says `Proposal::SelfRemove` "becomes reachable for the first
    /// time". **In Lakko it does not, yet** — [`Group::create`] leaves the
    /// OpenMLS wire-format policy at its pure-ciphertext default, and a
    /// `SelfRemove` must be sent as a `PublicMessage`, so OpenMLS refuses to
    /// craft one at all. The first half of this test pins that refusal, so the
    /// day someone changes the wire-format policy (a wire-visible change: it
    /// would stop encrypting handshake messages to the zero-trust DS) this
    /// named test fails and forces a look at the `ProposalKind::Leave` arm,
    /// which is gated and unit-tested but currently unreachable from the wire.
    ///
    /// The self-leave route ADR-0045 **does** open is a plain `Remove` naming
    /// the proposer's own leaf, and the second half proves the gate's owner
    /// exception holds on it: the owner cannot leave, by any route.
    #[test]
    fn self_removal_is_gated_for_its_own_sender_and_refused_for_the_owner() {
        let mut s = scene(0x05);

        let refusal = s
            .charlie_g
            .inner
            .leave_group_via_self_remove(&s.charlie.provider, &s.charlie.signature_keys)
            .expect_err("SelfRemove is unreachable under a pure-ciphertext policy");
        assert!(
            format!("{refusal:?}").contains("CannotSelfRemoveWithPureCiphertext"),
            "expected the wire-format refusal, got {refusal:?}",
        );

        // The reachable route: a member proposes the removal of its own leaf.
        let own = s.charlie_g.own_leaf_index();
        let leave = s
            .charlie_g
            .propose_remove_member(&s.charlie, own)
            .expect("a member may propose its own removal");
        let incoming = s
            .bob_g
            .process_incoming_verifying_roles(&s.bob, &leave)
            .expect("bob processes it");
        let Incoming::Proposal(record) = incoming else {
            panic!("expected a proposal, got {incoming:?}");
        };
        assert!(record.stored, "a member's self-removal must be stored");
        assert_eq!(
            record.sender_leaf,
            Some(own.u32()),
            "attributed to its own sender, and to no one else",
        );
        assert_eq!(
            record.kind,
            ProposalKind::Remove {
                target_leaf: own.u32()
            },
        );

        // The OWNER cannot take the same route — an ownerless group could never
        // appoint another admin again (`set_group_roles` is owner-only).
        let alice_own = s.alice_g.own_leaf_index();
        assert!(
            s.alice_g
                .propose_remove_member(&s.alice, alice_own)
                .is_err(),
            "the owner must not be able to leave",
        );
    }

    /// **Control for the HIGH-1 guard.** Reproduces the hazard on the path the
    /// guard now closes, so the property test that asserts the guard works is
    /// provably measuring something real.
    ///
    /// An admin removes a member while an ownership transfer is pending.
    /// Without the guard the removal's commit also carries the transfer, and
    /// the pair leaves the group with an owner it is removing — so
    /// `owner_survives` refuses it on every peer, while its author has already
    /// merged. Committer at N+1, peers at N, silently and permanently: the
    /// exact G-38 signature ADR-0045 exists to erase, through a path that
    /// predates it.
    #[test]
    fn control_a_direct_commit_that_absorbs_a_pending_proposal_forks_the_committer() {
        let mut s = scene(0x0B);

        let transfer = s
            .alice_g
            .propose_ownership_transfer(
                &s.alice,
                s.charlie.signature_public_key(),
                OutgoingOwner::Admin,
            )
            .expect("alice proposes handing the group to charlie");
        s.bob_g
            .process_incoming_verifying_roles(&s.bob, &transfer)
            .expect("bob stores the transfer");
        assert_eq!(s.bob_g.pending_proposals().len(), 1);

        let charlie_leaf = LeafNodeIndex::new(
            s.bob_g
                .members()
                .iter()
                .find(|m| m.signature_public_key == s.charlie.signature_public_key())
                .expect("charlie is a member")
                .leaf_index,
        );
        let epoch_before = s.alice_g.epoch();

        // The pre-guard behaviour: the removal sweeps the transfer in with it.
        let commit = s
            .bob_g
            .ungated_remove_member(&s.bob, charlie_leaf)
            .expect("the ungated path builds and merges it");
        assert_eq!(
            s.bob_g.epoch(),
            epoch_before + 1,
            "the committer merged eagerly, before anyone judged it",
        );

        assert!(
            s.alice_g
                .process_incoming_verifying_roles(&s.alice, &wire(commit))
                .is_err(),
            "every peer refuses a commit that removes the owner it installs",
        );
        assert_eq!(s.alice_g.epoch(), epoch_before, "so the peer stays at N");
        assert_ne!(
            s.alice_g.epoch(),
            s.bob_g.epoch(),
            "and the committer is alone at N+1 — the fork the guard prevents",
        );
    }

    /// **Sentinel for the park.** Pins the upstream behaviour
    /// [`Group::create_application_message`] works around: OpenMLS refuses
    /// `MlsGroup::create_message` outright while its proposal store is
    /// non-empty. Driven against the RAW OpenMLS handle rather than the Lakko
    /// wrapper, so it measures the library and not our own code.
    ///
    /// If a future OpenMLS bump drops this refusal, this test fails — and that
    /// failure is the signal that the park has become dead weight and may be
    /// deleted, rather than a regression.
    #[test]
    fn openmls_still_refuses_to_send_while_a_proposal_is_pending() {
        let mut s = scene(0x0A);
        let dave = Identity::from_seed(&[0x7Au8; 64]).expect("dave");

        let proposal = s
            .bob_g
            .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
            .expect("bob proposes add-dave");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &proposal)
            .expect("alice stores it");
        assert_eq!(s.alice_g.pending_proposals().len(), 1);

        let raw = s.alice_g.inner.create_message(
            &s.alice.provider,
            &s.alice.signature_keys,
            b"straight at OpenMLS",
        );
        assert!(
            matches!(
                raw,
                Err(openmls::prelude::CreateMessageError::GroupStateError(
                    openmls::group::MlsGroupStateError::PendingProposal
                ))
            ),
            "upstream must still refuse; got {raw:?}",
        );

        // And the Lakko wrapper, over the same group, does not.
        s.alice_g
            .create_application_message(&s.alice, b"through the park")
            .expect("the wrapper parks the pending set and sends");
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            1,
            "the park must put the pending set back exactly as it was",
        );
    }
}

// ============================================================================
// ADR-0027 external-join approval — property tests (G-3)
// ============================================================================

#[cfg(test)]
mod external_join_tests {
    use super::*;
    use crate::group_roles::RoleTable;
    use crate::key_package;

    fn wire(m: MlsMessageOut) -> Vec<u8> {
        encode_message(&m).expect("encode")
    }

    fn kp_wire(id: &Identity) -> Vec<u8> {
        key_package::serialize(&id.create_key_package().expect("kp")).expect("serialize kp")
    }

    fn pk(id: &Identity) -> Vec<u8> {
        id.signature_public_key().to_vec()
    }

    fn pk32(id: &Identity) -> [u8; 32] {
        id.signature_public_key()
            .try_into()
            .expect("32-byte signature key")
    }

    /// The joiner-side primitive: a non-member crafts an external Add proposal
    /// for a group's current id + epoch (ADR-0027 G-3). Delegates to the public
    /// [`Identity::create_external_join_proposal`] so these tests exercise the
    /// shipped API. (The full join-ticket acquisition is the joiner-side
    /// Phase-4 work; here the test supplies the params directly.)
    fn external_join_proposal(joiner: &Identity, group_id: &[u8], epoch: u64) -> Vec<u8> {
        joiner
            .create_external_join_proposal(group_id, epoch)
            .expect("join proposal")
    }

    #[test]
    fn owner_approves_external_join_adds_joiner() {
        // Open group (no role table) — approve is allowed for any member.
        let alice = Identity::from_seed(&[0xA0u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice).expect("create");
        let dave = Identity::from_seed(&[0xD0u8; 64]).expect("dave");
        let dave_pk = pk(&dave);

        let prop = external_join_proposal(&dave, &alice_g.group_id(), alice_g.epoch());
        let incoming = alice_g.process_incoming(&alice, &prop).expect("process");
        assert!(
            matches!(&incoming, Incoming::ExternalJoinProposal { joiner_signature_key }
                if *joiner_signature_key == dave_pk),
            "got {incoming:?}",
        );
        assert_eq!(alice_g.pending_external_joins(), vec![dave_pk.clone()]);

        alice_g
            .approve_external_join(&alice, &dave_pk)
            .expect("approve");
        assert!(
            alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave_pk),
            "dave should be a member after approve",
        );
        assert!(alice_g.pending_external_joins().is_empty());
    }

    #[test]
    fn reject_removes_without_adding() {
        let alice = Identity::from_seed(&[0xA2u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice).expect("create");
        let dave = Identity::from_seed(&[0xD2u8; 64]).expect("dave");
        let dave_pk = pk(&dave);

        let prop = external_join_proposal(&dave, &alice_g.group_id(), alice_g.epoch());
        alice_g.process_incoming(&alice, &prop).expect("process");
        assert_eq!(alice_g.pending_external_joins().len(), 1);

        alice_g.reject_external_join(&dave_pk).expect("reject");
        assert!(alice_g.pending_external_joins().is_empty());
        assert!(
            !alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave_pk),
            "dave must NOT be a member after reject",
        );
        // Epoch unchanged (no commit).
        assert_eq!(alice_g.epoch(), 0);
    }

    #[test]
    fn external_join_dedup_by_signature_key() {
        let alice = Identity::from_seed(&[0xA3u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice).expect("create");
        let dave = Identity::from_seed(&[0xD3u8; 64]).expect("dave");

        let p1 = external_join_proposal(&dave, &alice_g.group_id(), alice_g.epoch());
        alice_g.process_incoming(&alice, &p1).expect("p1");
        let p2 = external_join_proposal(&dave, &alice_g.group_id(), alice_g.epoch());
        alice_g.process_incoming(&alice, &p2).expect("p2");
        // Same joiner signature key → a single pending entry.
        assert_eq!(alice_g.pending_external_joins().len(), 1);
    }

    #[test]
    fn approve_unknown_ref_errors() {
        let alice = Identity::from_seed(&[0xA4u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice).expect("create");
        assert!(alice_g.approve_external_join(&alice, b"nope").is_err());
        assert!(alice_g.reject_external_join(b"nope").is_err());
    }

    #[test]
    fn non_admin_approve_refused() {
        // alice = owner, bob = plain member, role table set.
        let alice = Identity::from_seed(&[0xA5u8; 64]).expect("alice");
        let bob = Identity::from_seed(&[0xB5u8; 64]).expect("bob");
        let mut alice_g = Group::create(&alice).expect("create");

        let add = alice_g
            .add_member_from_wire(&alice, &kp_wire(&bob))
            .expect("add bob");
        let mut bob_g = Group::join_from_welcome(&wire(add.welcome), &bob).expect("bob joins");

        // alice (owner) sets the role table: herself owner, no admins.
        let roles = RoleTable::new(pk32(&alice), vec![]);
        let role_commit = alice_g.set_group_roles(&alice, &roles).expect("set roles");
        bob_g
            .process_incoming_verifying_roles(&bob, &role_commit)
            .expect("bob applies roles");

        // A non-member asks to join; bob (a plain member) tries to approve.
        let dave = Identity::from_seed(&[0xD5u8; 64]).expect("dave");
        let dave_pk = pk(&dave);
        let prop = external_join_proposal(&dave, &bob_g.group_id(), bob_g.epoch());
        bob_g.process_incoming(&bob, &prop).expect("bob processes");
        assert_eq!(bob_g.pending_external_joins(), vec![dave_pk.clone()]);

        // Role-gated: a plain member is refused, no commit produced.
        assert!(
            bob_g.approve_external_join(&bob, &dave_pk).is_err(),
            "a non-admin must not approve an external join",
        );
        // dave still not a member.
        assert!(!bob_g
            .members()
            .iter()
            .any(|m| m.signature_public_key == dave_pk));
    }

    /// Full G-3 loop, in-process: a non-member crafts an external-join proposal
    /// via the public joiner API, an admin approves it, the resulting Welcome
    /// rides a [`crate::WelcomeRendezvous`] envelope, and the joiner matches it
    /// by its own signature key + opens the inner Welcome — landing as a member
    /// at the post-approve epoch. Proves the joiner side closes the loop
    /// end-to-end (ADR-0027 G-3).
    #[test]
    fn joiner_api_full_loop_proposes_approves_joins_via_rendezvous() {
        let alice = Identity::from_seed(&[0xA9u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice).expect("create");
        let alice_pk = pk(&alice);
        let dave = Identity::from_seed(&[0xD9u8; 64]).expect("dave");
        let dave_pk = pk(&dave);

        // Joiner side: craft the proposal via the public API.
        let prop = dave
            .create_external_join_proposal(&alice_g.group_id(), alice_g.epoch())
            .expect("dave proposal");

        // Admin side: surface + approve.
        let incoming = alice_g.process_incoming(&alice, &prop).expect("process");
        assert!(
            matches!(&incoming, Incoming::ExternalJoinProposal { joiner_signature_key }
                if *joiner_signature_key == dave_pk),
            "got {incoming:?}",
        );
        let add = alice_g
            .approve_external_join(&alice, &dave_pk)
            .expect("approve");

        // Transport: wrap the Welcome in a rendezvous envelope (what the admin
        // posts to the group journal).
        let envelope = crate::WelcomeRendezvous {
            joiner_signature_key: dave_pk.clone(),
            welcome: encode_message(&add.welcome).expect("encode welcome"),
        }
        .encode()
        .expect("encode rendezvous");

        // Joiner side: match the envelope to our own key + open the inner Welcome.
        let decoded = crate::WelcomeRendezvous::decode(&envelope).expect("decode rendezvous");
        assert!(decoded.is_for(&dave_pk), "rendezvous must target dave");
        assert!(
            !decoded.is_for(&alice_pk),
            "rendezvous must not target alice"
        );
        let dave_g = Group::join_from_welcome(&decoded.welcome, &dave).expect("dave joins");

        // dave is now a member at alice's post-approve epoch; each is in the
        // other's roster.
        assert_eq!(dave_g.epoch(), alice_g.epoch());
        assert!(
            dave_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == alice_pk),
            "alice should be in dave's roster after join",
        );
        assert!(
            alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave_pk),
            "dave should be in alice's roster after approve",
        );
    }

    /// ADR-0028 §3 — a non-member can read a group's current epoch from the
    /// public MLS framing of a journal message (no group secret), which is what
    /// lets the join-ticket joiner craft an external-join proposal for the right
    /// epoch.
    #[test]
    fn epoch_of_message_reads_public_framing() {
        let alice = Identity::from_seed(&[0xE0u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice).expect("create"); // epoch 0
        let bob = Identity::from_seed(&[0xE1u8; 64]).expect("bob");

        // An application message at epoch 0 reports epoch 0 from its framing.
        let m0 = alice_g
            .create_application_message(&alice, b"hello")
            .expect("app msg");
        assert_eq!(crate::epoch_of_message(&wire(m0)), Some(0));

        // Add bob → alice merges → epoch advances. The Welcome has no epoch.
        let add = alice_g
            .add_member_from_wire(&alice, &kp_wire(&bob))
            .expect("add bob");
        assert_eq!(crate::epoch_of_message(&wire(add.welcome)), None);

        // A fresh app message now reports the advanced epoch.
        let m1 = alice_g
            .create_application_message(&alice, b"world")
            .expect("app msg 2");
        assert_eq!(alice_g.epoch(), 1);
        assert_eq!(crate::epoch_of_message(&wire(m1)), Some(alice_g.epoch()));

        // Non-MLS / empty bytes → None (the caller treats this as epoch 0).
        assert_eq!(crate::epoch_of_message(&[0xFFu8; 12]), None);
        assert_eq!(crate::epoch_of_message(&[]), None);
    }
}
