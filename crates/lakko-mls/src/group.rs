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
    /// A standalone proposal was processed (no state advance).
    Proposal,
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
    /// * add a member               -> committer is owner or admin
    /// * remove a non-owner member  -> committer is owner or admin
    /// * remove the owner           -> refused (the owner is unremovable)
    /// * remove only your own leaf  -> always allowed (leaving)
    /// * change the role table       -> committer is the owner only
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the committer's role does not authorise the commit
    /// (left un-merged), plus the usual decode / process errors.
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
                    let committer_pubkey: Option<Vec<u8>> = committer_leaf.and_then(|leaf| {
                        members
                            .iter()
                            .find(|m| m.leaf_index == leaf)
                            .map(|m| m.signature_public_key.clone())
                    });
                    authorise_role_commit(&table, &members, committer_pubkey.as_deref(), &staged)?;
                }
                self.inner
                    .merge_staged_commit(&identity.provider, *staged)
                    .map_err(|e| Error::Group(format!("merge_staged_commit: {e:?}")))?;
                Ok(Incoming::Commit)
            },
            ProcessedMessageContent::ProposalMessage(_) => Ok(Incoming::Proposal),
            ProcessedMessageContent::ExternalJoinProposalMessage(qp) => {
                self.retain_external_join(&qp)
            },
        }
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
            ProcessedMessageContent::ProposalMessage(_) => Ok(Incoming::Proposal),
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
            ProcessedMessageContent::ProposalMessage(_) => Ok(Incoming::Proposal),
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
/// merged) when the committer's role does not authorise the commit's effect.
/// `committer_pubkey` is the committer leaf's pre-merge signature key (`None`
/// for an external / unresolved sender — treated as unauthorised).
///
/// Note on "leaving": MLS (OpenMLS `CannotRemoveSelf`) forbids a member from
/// committing the removal of their **own** leaf, so a committer is never inside
/// their own commit's removed set — a self-Remove-by-self never reaches this
/// gate. A member leaves by an admin/owner committing the removal (authorised
/// here normally) or, as a follow-up, a self-remove proposal an admin commits.
fn authorise_role_commit(
    table: &RoleTable,
    members: &[MemberRecord],
    committer_pubkey: Option<&[u8]>,
    staged: &StagedCommit,
) -> Result<()> {
    let has_add = staged.add_proposals().next().is_some();
    let removed_leaves: Vec<LeafNodeIndex> = staged
        .remove_proposals()
        .map(|qrp| qrp.remove_proposal().removed())
        .collect();
    let removes_owner = removed_leaves.iter().any(|leaf| {
        members
            .iter()
            .find(|m| m.leaf_index == leaf.u32())
            .is_some_and(|m| table.is_owner(&m.signature_public_key))
    });
    let has_role_change = staged
        .queued_proposals()
        .any(|qp| matches!(qp.proposal(), Proposal::GroupContextExtensions(_)));

    let committer_is_owner = committer_pubkey.is_some_and(|pk| table.is_owner(pk));
    let committer_can_admin = committer_pubkey.is_some_and(|pk| table.can_administer(pk));

    // A role-table change (GroupContextExtensions) is owner-only.
    if has_role_change && !committer_is_owner {
        return Err(Error::Group(
            "role-table change refused: committer is not the owner".into(),
        ));
    }
    // The owner is unremovable, by anyone.
    if removes_owner {
        return Err(Error::Group(
            "removal refused: the group owner cannot be removed".into(),
        ));
    }
    // Any other removal, and any add, needs owner / admin.
    if !removed_leaves.is_empty() && !committer_can_admin {
        return Err(Error::Group(
            "removal refused: committer is not owner or admin".into(),
        ));
    }
    if has_add && !committer_can_admin {
        return Err(Error::Group(
            "add refused: committer is not owner or admin".into(),
        ));
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
