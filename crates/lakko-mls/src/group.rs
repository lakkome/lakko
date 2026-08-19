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
use openmls_traits::OpenMlsProvider as _;
use tls_codec::{Deserialize as _, Serialize as _};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::group_proposals::{
    authorise, classify, pubkey_at_leaf, ProposalKind, ProposalRecord, Proposer,
};
use crate::group_roles::{Role, RoleTable, ROLE_EXTENSION_TYPE};
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

    /// Add a member by their published [`KeyPackage`].
    ///
    /// Consumes one of the new member's published KeyPackages on success.
    /// The local group state is advanced to the new epoch atomically.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for any OpenMLS-side failure (validation,
    /// signature, storage).
    pub fn add_member(
        &mut self,
        identity: &Identity,
        key_package: KeyPackage,
    ) -> Result<MemberAddition> {
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
    /// Returns [`Error::Group`] for any OpenMLS-side failure.
    pub fn self_update(&mut self, identity: &Identity) -> Result<Vec<u8>> {
        use openmls::prelude::LeafNodeParameters;
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
    /// Returns [`Error::Group`] for any OpenMLS-side failure.
    pub fn remove_member(
        &mut self,
        identity: &Identity,
        leaf: LeafNodeIndex,
    ) -> Result<MlsMessageOut> {
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
    /// [`Error::Group`] on the OpenMLS update / merge (e.g. a member whose
    /// leaf capabilities do not advertise an added extension type).
    pub fn set_group_context_extensions(
        &mut self,
        identity: &Identity,
        extensions: Extensions<GroupContext>,
    ) -> Result<MlsMessageOut> {
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
    /// [`Error::Group`] if the caller is not owner/admin, if no pending proposal
    /// matches `proposal_ref`, or on an OpenMLS add failure.
    pub fn approve_external_join(
        &mut self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<MemberAddition> {
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

    /// Rewrite the group's ADR-0022 role table (promote / demote admins).
    /// Produces a `GroupContextExtensions` commit the caller broadcasts to the
    /// group journal. **Authorisation is enforced on the RECEIVING side** by
    /// [`process_incoming_verifying_roles`](Self::process_incoming_verifying_roles)
    /// — only the owner's role-change commit is merged. This method just
    /// produces the commit, so a non-owner who calls it has their commit
    /// refused by every conformant peer.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] on any OpenMLS-side failure.
    pub fn set_group_roles(&mut self, identity: &Identity, roles: &RoleTable) -> Result<Vec<u8>> {
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
    fn authorise_own_proposal(&self, identity: &Identity, kind: &ProposalKind) -> Result<()> {
        let table = self.role_table_or_refuse("proposal")?;
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
    /// not the owner, or on any OpenMLS-side failure.
    pub fn propose_group_roles(
        &mut self,
        identity: &Identity,
        roles: &RoleTable,
    ) -> Result<Vec<u8>> {
        self.authorise_own_proposal(identity, &ProposalKind::RoleChange)?;
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
        let table = self.role_table_or_refuse("sweep")?;
        if !table.can_administer(identity.signature_public_key()) {
            return Err(Error::Group(
                "sweep refused: this device is not owner or admin".into(),
            ));
        }
        let members = self.members();

        // Nothing authorised pending => emit no commit at all. This is a
        // prediction of what the filter below will keep, and it is used ONLY to
        // decide emptiness; `swept` is read back off the staged commit
        // afterwards so the caller is told what the commit really carried
        // rather than what we expected it to.
        let nothing_to_sweep = !self.inner.pending_proposals().any(|qp| {
            let leaf = match qp.sender() {
                Sender::Member(idx) => Some(idx.u32()),
                _ => None,
            };
            let proposer = leaf.and_then(|leaf| {
                pubkey_at_leaf(&members, leaf).map(|pubkey| Proposer { leaf, pubkey })
            });
            authorise(&table, &members, proposer, &classify(qp.proposal())).is_ok()
        });
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
                |qp| {
                    let leaf = match qp.sender() {
                        Sender::Member(idx) => Some(idx.u32()),
                        _ => None,
                    };
                    let proposer = leaf.and_then(|leaf| {
                        pubkey_at_leaf(&filter_members, leaf)
                            .map(|pubkey| Proposer { leaf, pubkey })
                    });
                    authorise(
                        &filter_table,
                        &filter_members,
                        proposer,
                        &classify(qp.proposal()),
                    )
                    .is_ok()
                },
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
        self.inner
            .merge_pending_commit(&identity.provider)
            .map_err(|e| Error::Group(format!("merge_pending_commit: {e:?}")))?;

        Ok(Some(ProposalCommit {
            commit,
            welcome,
            swept,
        }))
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
    /// # Errors
    ///
    /// Returns [`Error::Message`] on encryption failure.
    pub fn create_application_message(
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
