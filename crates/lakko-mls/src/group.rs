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
//! ([`Group::add_member`], [`Group::remove_member`], [`Group::set_group_roles`])
//! is the original one and is destined to go (ADR-0045). The **proposal** shape
//! that replaces it is [`Group::propose_add_member`] /
//! [`Group::propose_remove_member`] / [`Group::propose_group_roles`] /
//! [`Group::approve_external_join`], with one designated owner-or-admin — see
//! [`crate::group_roles::committer_rank`] — turning the pending set into a
//! single Commit via [`Group::commit_pending_proposals`]. The authorisation
//! rule both shapes are judged by lives in [`crate::group_proposals`].
//!
//! Since ADR-0045 **S7** the external-join approve is on the proposal side, and
//! it is the last membership path to move: a role-enabled group has no
//! direct-commit route into its own membership at all. The direct approve
//! survives only where a role table — and therefore a committer — cannot exist,
//! reachable as [`crate::PublisherGroup::approve_external_join`] and nowhere
//! else.
//!
//! Ownership moves with [`Group::transfer_ownership`] /
//! [`Group::propose_ownership_transfer`] (ADR-0022's deferred follow-up), which
//! are validated wrappers over the same role-table rewrite rather than a new
//! commit kind. They are what makes leaving possible at all: an owner may not
//! leave while owner, so without transfer a person was bound to every group they
//! created.

use openmls::ciphersuite::hash_ref::ProposalRef;
use openmls::credentials::CredentialType;
use openmls::extensions::{
    Extension, ExtensionType, Extensions, RequiredCapabilitiesExtension, UnknownExtension,
};
use openmls::framing::{MlsMessageBodyIn, MlsMessageIn, MlsMessageOut, Sender};
use openmls::group::{
    GroupContext, MlsGroup, MlsGroupCreateConfig, MlsGroupJoinConfig, QueuedProposal, StagedCommit,
    StagedWelcome,
};
use openmls::messages::proposals::{Proposal, ProposalType};
use openmls::prelude::{
    Ciphersuite, KeyPackage, LeafNodeIndex, ProcessedMessageContent, ProtocolVersion,
};
use openmls::treesync::EncryptionKey;
use openmls_rust_crypto::MemoryStorage;
use openmls_traits::types::VerifiableCiphersuite;
use openmls_traits::OpenMlsProvider as _;
use tls_codec::{Deserialize as _, Serialize as _};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::group_kind::{GroupKind, KindState, GROUP_KIND_EXTENSION_TYPE};
use crate::group_proposals::{
    account_key_at_leaf, authorise, classify, commit_effects, group_kind_unchanged,
    is_owner_response, leaves_of_account, one_to_one_allows, one_to_one_holds, owner_survives,
    proposal_keeps_group_kind, proposer_at, CommitEffects, ProposalKind, ProposalRecord, Proposer,
    MAX_LEAVES_PER_ACCOUNT,
};
use crate::group_roles::{OutgoingOwner, Role, RoleTable, ROLE_EXTENSION_TYPE};
use crate::identity::Identity;
use crate::refusal::RefusalReason;

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

/// The MLS protocol version every Lakko group runs at.
///
/// Restated as a constant because OpenMLS publishes **no** accessor for a live
/// group's version — `MlsGroup::export_group_context` is `#[cfg(test-utils)]`
/// and `PublicGroup::version` is `pub(crate)` — while
/// [`added_leaf_can_join`] needs it to re-validate a KeyPackage the way
/// `validate_add_proposals` would.
///
/// It is not a guess. [`Group::create`] builds every group through
/// `MlsGroupCreateConfig`, whose protocol version is
/// `ProtocolVersion::default()` — the value below, and the only version
/// openmls 0.8.1 implements. The same constant is already pinned at the two
/// places a wire KeyPackage is admitted ([`Group::add_member_from_wire`] and
/// [`Group::propose_add_member_from_wire`]), so a version this crate does not
/// name would fail there long before it reached a sweep.
const GROUP_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::Mls10;

/// Max external-join proposals retained per group awaiting an owner/admin
/// decision (ADR-0027). A proposal arriving past this cap is surfaced but not
/// retained — anti-spam, since a non-member can post these.
const MAX_PENDING_EXTERNAL_JOINS: usize = 64;

/// Storage-provider row under which a group's retained external-join
/// requests are persisted (ADR-0027, the "persistence is a follow-up" of the
/// original v1).
///
/// The row lives in the same key-value provider OpenMLS writes the group
/// state into, so it rides [`Identity::snapshot_storage`] /
/// [`Identity::restore_storage`] unchanged and reaches every client's
/// device-key-wrapped snapshot with no client-side store change. The key is
/// `DOMAIN ‖ group_id`; the NUL keeps it out of any OpenMLS label prefix,
/// which are all printable.
///
/// **What this closes.** Before it, the queue was a plain `Vec` on the
/// `Group`, so an owner whose app restarted while a `/g/` request was
/// pending forgot the request, and the joiner — whose landing says "Request
/// sent, waiting for an admin" with no deadline (ADR-0045 decision 2) — waited
/// forever for a decision nobody could make any more.
const PENDING_EXTERNAL_JOINS_ROW_DOMAIN: &[u8] = b"lakko/pending-external-joins/v1\0";

/// Version byte of the row body (`version ‖ count ‖ [len(4 BE) ‖ KeyPackage]*`).
const PENDING_EXTERNAL_JOINS_ROW_VERSION: u8 = 0x01;

/// Upper bound on one retained KeyPackage's wire size inside the row. A
/// real one is a few hundred bytes; the bound exists so a corrupt length
/// prefix cannot ask for an allocation the row does not carry.
const MAX_PENDING_KEY_PACKAGE_BYTES: usize = 16 * 1024;

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
/// **That reasoning had a hole, and it is worth keeping the shape of it.** It
/// enumerated what a plain member may *ask for* and concluded the flood was
/// de-duplicable, but de-duplication is a property of the **wire bytes**, not of
/// the ask. `Proposal::Update` was open to every member and carries a freshly
/// generated leaf node, so N of them are N distinct `ProposalRef`s that nothing
/// collapses — a plain member could fill this cap on every peer with stock
/// proposals, freezing the group's membership until a sweep and re-flooding
/// immediately after. `authorise` now refuses `Update` outright (see its
/// documentation for the second, sharper reason), so the cap is once again
/// reachable only from an ask that is either de-duplicable or owner/admin-only.
///
/// **And that patch left a second hole, of the same shape.** The residual —
/// `Add`, which mints a fresh `ProposalRef` every time — was dismissed as
/// "owner-or-admin only, a role that can already remove every member, so filling
/// this set is strictly weaker than what it already holds". It is not: the two
/// capabilities **compose**. Mass removal is loud, attributable and repairable;
/// filling this set removes the owner's *response*, because at the cap
/// [`Group::authorise_own_proposal`] refuses the owner's own removal and role
/// change too, [`Group::refuse_if_proposals_pending`] refuses every direct
/// commit, and nothing discards a pending proposal without committing it. The
/// only exit was to sweep, which enacts the flood and leaves the flooder free to
/// re-flood at once. `Remove` reaches the same place by a second route that the
/// de-duplication argument above does not cover: a repeat naming *one* target
/// collapses, but one member may name every *other* member, and those do not.
/// The fix is [`MAX_RESERVED_OWNER_RESPONSES`], not a smaller number here.
///
/// **At the cap the late proposer pays, never the group.** A proposal arriving
/// into a full set is refused (`stored == false`) rather than evicting one
/// already held — evicting would be strictly worse than the flood, because a
/// commit that references an evicted proposal can no longer be resolved and is
/// refused with `InvalidCommit(MissingProposal)` on the evicting member alone.
/// Refusing the newcomer costs it a re-proposal after the next sweep; evicting
/// an incumbent would cost the whole group its next commit.
pub(crate) const MAX_PENDING_PROPOSALS: usize = 64;

/// Slots reserved, **past** [`MAX_PENDING_PROPOSALS`], for the group owner's
/// `Remove` and `RoleChange` proposals.
///
/// The owner must never lose the ability to remove a member or change roles, and
/// a shared cap enforced without regard to who is asking hands exactly that away
/// to anyone who can fill it. The reserve is the answer: a proposal that
/// [`crate::group_proposals::is_owner_response`] recognises is admitted while
/// the set is below `MAX_PENDING_PROPOSALS + MAX_RESERVED_OWNER_RESPONSES`,
/// everything else while it is below `MAX_PENDING_PROPOSALS`. See that function
/// for the escalation in full and for why the reserve grants no new authority.
///
/// **The real bound on the pending set becomes the sum, 68.** The set is cloned
/// and re-stored once per outgoing chat message and is persisted, so what
/// matters is that it stays finite and small; four slots on top of sixty-four is
/// a 6% ceiling increase for an unconditional guarantee.
///
/// **Why four.** One would do for the mechanism: a role table is rewritten
/// wholesale, so a single `RoleChange` demotes every malicious admin at once,
/// and [`Group::refuse_if_role_change_pending`] caps role changes at one pending
/// anyway. Four lets the owner also file a few removals in the same round
/// without waiting for a sweep, while keeping the owner's own share bounded —
/// the reserve must not become an unbounded channel merely because the principal
/// spending it is trusted. Each sweep empties the whole set, so this is a
/// per-round budget, not a lifetime one.
pub(crate) const MAX_RESERVED_OWNER_RESPONSES: usize = 4;

/// How full the pending set may be before this proposal is refused.
///
/// The single place the two-tier cap is expressed, consumed by both
/// [`Group::authorise_own_proposal`] and [`Group::gate_incoming_proposal`]. They
/// must agree exactly: an owner whose response its peers refuse to retain would
/// sweep a commit those peers cannot resolve
/// (`InvalidCommit(MissingProposal)`), which is a worse outcome than the refusal
/// it was working around.
const fn pending_capacity(is_owner_response: bool) -> usize {
    if is_owner_response {
        MAX_PENDING_PROPOSALS + MAX_RESERVED_OWNER_RESPONSES
    } else {
        MAX_PENDING_PROPOSALS
    }
}

/// A non-member's external-join request retained for an owner/admin decision
/// (ADR-0027, G-3). `joiner_signature_key` is the canonical identity handle the
/// rest of `lakko-mls` keys on (the role table + the contact graph match on
/// it); it doubles as the opaque ref the FFI / UI pass back to
/// [`Group::approve_external_join`] / [`Group::reject_external_join`].
/// `key_package` is the joiner's KeyPackage that an approve adds via the
/// existing, audited [`Group::add_member`] path.
pub(crate) struct PendingExternalJoin {
    pub(crate) joiner_signature_key: Vec<u8>,
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
    /// reject. Bounded by [`MAX_PENDING_EXTERNAL_JOINS`].
    ///
    /// The `Vec` is the working copy; the durable copy is the
    /// [`PENDING_EXTERNAL_JOINS_ROW_DOMAIN`] row in the identity's storage
    /// provider, rewritten by every mutation
    /// ([`Self::persist_pending_external_joins`]) and read back by
    /// [`Group::load`] — so a request survives the owner's app restart, which
    /// is the one thing the in-memory v1 could not offer.
    pending_external_joins: Vec<PendingExternalJoin>,
}

/// The provider row key for `group_id`'s retained external-join requests.
fn pending_external_joins_row_key(group_id: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(PENDING_EXTERNAL_JOINS_ROW_DOMAIN.len() + group_id.len());
    key.extend_from_slice(PENDING_EXTERNAL_JOINS_ROW_DOMAIN);
    key.extend_from_slice(group_id);
    key
}

/// Encode the retained requests as the row body. Only the KeyPackages are
/// written: the signature key is re-derived from each package's leaf on
/// decode, so the two can never disagree on disk.
///
/// # Errors
///
/// [`Error::KeyPackage`] if a retained package fails to serialize, or
/// [`Error::Group`] if the set somehow exceeds what a one-byte count or a
/// four-byte length can carry (unreachable under the retention cap).
fn encode_pending_external_joins(pending: &[PendingExternalJoin]) -> Result<Vec<u8>> {
    let count = u8::try_from(pending.len())
        .map_err(|_| Error::Group("pending external joins exceed the row count".into()))?;
    let mut out = vec![PENDING_EXTERNAL_JOINS_ROW_VERSION, count];
    for p in pending {
        let kp = crate::key_package::serialize(&p.key_package)?;
        let len = u32::try_from(kp.len())
            .map_err(|_| Error::Group("retained KeyPackage exceeds the row length".into()))?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&kp);
    }
    Ok(out)
}

/// Decode a row body written by [`encode_pending_external_joins`], bounded
/// and fail-closed: an unknown version, a count over
/// [`MAX_PENDING_EXTERNAL_JOINS`], a length over
/// [`MAX_PENDING_KEY_PACKAGE_BYTES`], a truncated body, trailing bytes, or a
/// KeyPackage that no longer validates against this identity's ciphersuite
/// all refuse the whole row. Each package is re-validated exactly as
/// [`Group::add_member_from_wire`] validates one off the wire, so what comes
/// back from disk is never weaker than what was retained.
///
/// # Errors
///
/// [`Error::Group`] on a framing violation, [`Error::KeyPackage`] on a
/// package that fails to parse or validate.
pub(crate) fn decode_pending_external_joins(
    bytes: &[u8],
    identity: &Identity,
) -> Result<Vec<PendingExternalJoin>> {
    let (&version, rest) = bytes
        .split_first()
        .ok_or_else(|| Error::Group("pending external joins row is empty".into()))?;
    if version != PENDING_EXTERNAL_JOINS_ROW_VERSION {
        return Err(Error::Group(format!(
            "pending external joins row version {version} is not {PENDING_EXTERNAL_JOINS_ROW_VERSION}"
        )));
    }
    let (&count, mut cur) = rest
        .split_first()
        .ok_or_else(|| Error::Group("pending external joins row has no count".into()))?;
    let count = usize::from(count);
    if count > MAX_PENDING_EXTERNAL_JOINS {
        return Err(Error::Group(format!(
            "pending external joins row counts {count}, over the cap of {MAX_PENDING_EXTERNAL_JOINS}"
        )));
    }
    let mut pending = Vec::with_capacity(count);
    for _ in 0..count {
        if cur.len() < 4 {
            return Err(Error::Group(
                "pending external joins row truncated at a length".into(),
            ));
        }
        let (len_bytes, tail) = cur.split_at(4);
        let mut len_arr = [0u8; 4];
        len_arr.copy_from_slice(len_bytes);
        let len = u32::from_be_bytes(len_arr) as usize;
        if len == 0 || len > MAX_PENDING_KEY_PACKAGE_BYTES {
            return Err(Error::Group(format!(
                "pending external joins row names a {len}-byte KeyPackage"
            )));
        }
        if tail.len() < len {
            return Err(Error::Group(
                "pending external joins row truncated in a KeyPackage".into(),
            ));
        }
        let (kp_bytes, tail) = tail.split_at(len);
        cur = tail;
        let key_package = crate::key_package::deserialize(kp_bytes)?
            .validate(identity.provider.crypto(), GROUP_PROTOCOL_VERSION)
            .map_err(|e| Error::KeyPackage(format!("retained KeyPackage validate: {e:?}")))?;
        let joiner_signature_key = key_package.leaf_node().signature_key().as_slice().to_vec();
        if pending
            .iter()
            .any(|p: &PendingExternalJoin| p.joiner_signature_key == joiner_signature_key)
        {
            return Err(Error::Group(
                "pending external joins row repeats a joiner".into(),
            ));
        }
        pending.push(PendingExternalJoin {
            joiner_signature_key,
            key_package,
        });
    }
    if !cur.is_empty() {
        return Err(Error::Group(
            "pending external joins row has trailing bytes".into(),
        ));
    }
    Ok(pending)
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
    /// 32-byte Ed25519 verifying key of the **leaf**.
    pub signature_public_key: Vec<u8>,
    /// The account this leaf belongs to (ADR-0050): the device binding's
    /// account key, or the leaf key itself for an unbound Phase 1 leaf. Empty
    /// when the credential resolved to nothing — such a leaf should never have
    /// been admitted, and every gate treats it as unauthorised.
    pub account_key: Vec<u8>,
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
        /// The account the sender leaf resolves to (ADR-0050), or `None` for an
        /// external sender or an unresolvable leaf. This — not the leaf, not
        /// the leaf key — is what authorship, reactions and read state key on,
        /// so a person is one author across all their devices.
        sender_account: Option<Vec<u8>>,
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
    /// A non-member's external join proposal was processed, and — if the
    /// stranger quarantine had room — retained for an owner/admin decision
    /// (ADR-0027, G-3). `joiner_signature_key` is the prospective joiner's leaf
    /// signature public key: the stable handle the caller passes to
    /// [`Group::approve_external_join`] / [`Group::reject_external_join`], and
    /// the key the role table / contact graph resolve to a display name.
    ExternalJoinProposal {
        /// The prospective joiner's leaf signature public key.
        joiner_signature_key: Vec<u8>,
        /// `true` when the request was retained and an approve can act on it.
        ///
        /// **`false` is the signal that an approve would fail, and a client
        /// must not offer one.** The event fires either way — a request that
        /// arrived while the quarantine was at
        /// [`MAX_PENDING_EXTERNAL_JOINS`] is surfaced but not kept, because a
        /// stranger must not be able to make a member's receive loop error and
        /// there is nowhere to put it. Without this flag the drop was
        /// completely silent: the admin saw a pending request in its list,
        /// tapped Approve, and got "no pending external join for that key" for
        /// an entry that had never existed.
        ///
        /// The remedy is the joiner's, not the admin's: ask again once the
        /// quarantine has drained. So a client renders an unretained request as
        /// informational at most, and never as actionable.
        ///
        /// This is deliberately the same shape as
        /// [`ProposalRecord::stored`] — a refusal that changes no state is data
        /// rather than an error, so the caller can render it instead of
        /// discovering it by failing.
        retained: bool,
    },
}

impl Group {
    /// Create a new MLS group with the given identity as the founding member.
    ///
    /// `kind` is written into the GroupContext as the `0xFF03` extension
    /// ([ADR-0060]) and never changes: a [`GroupKind::OneToOne`] keeps its two
    /// people for life, a [`GroupKind::Group`] follows the ordinary role and
    /// proposal rules even while it holds two people. It is a required argument
    /// so no caller can forget it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for any OpenMLS-side failure.
    ///
    /// [ADR-0060]: ../../../docs/adr/0060-conversation-kind.md
    pub fn create(identity: &Identity, kind: GroupKind) -> Result<Self> {
        // ADR-0022 (owner-bootstrap option A) — a conversation group is
        // role-enabled from creation with the creator as owner. The initial
        // GroupContext carries the `0xFF02` role table (owner = creator, no
        // admins) plus the required-capabilities declaration that lets a later
        // `set_group_roles` commit rewrite it (promote / demote). Seeding the
        // owner at creation closes the bootstrap window — there is never a
        // moment when the group has no owner for a member to claim.
        // ADR-0050: the owner slot names the creator's ACCOUNT key (the device
        // binding's account, or the identity's own key when it is unbound), so
        // every device of the creator is the owner.
        let owner: [u8; 32] = identity
            .account_public_key()
            .ok_or_else(|| Error::Group("identity signature key is not 32 bytes".into()))?;
        let initial = RoleTable::owner_only(owner);
        // ADR-0060: the kind rides beside the role table, and both are named
        // in `required_capabilities`, so a device that does not understand the
        // kind cannot be added and so cannot fail to enforce it.
        let exts = Extensions::from_vec(vec![
            conversation_required_capabilities(),
            Extension::Unknown(ROLE_EXTENSION_TYPE, UnknownExtension(initial.encode())),
            kind.extension(),
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
        // ADR-0060: a conversation (a group carrying the ADR-0022 role table)
        // must say what kind it is, readably, or it is not joined — checked
        // before the group is written to storage, so a refused Welcome leaves
        // nothing behind. A device that cannot classify a conversation must
        // never have to guess (invariant #4 shape: refuse rather than degrade).
        refuse_unclassifiable_conversation(staged.group_context().extensions())?;
        let inner = staged
            .into_group(&identity.provider)
            .map_err(|e| Error::Group(format!("welcome -> group: {e:?}")))?;

        let group = Self::from_mls_group(inner);
        // ADR-0050: the joiner checks the roster it lands in. A leaf whose
        // credential resolves to no account was admitted by a gate that is
        // not Lakko's; refuse the group rather than reason about it.
        if group.members().iter().any(|m| m.account_key.is_empty()) {
            return Err(Error::refused(
                RefusalReason::DeviceBindingInvalid,
                "welcome refused: a member's device binding does not verify",
            ));
        }
        Ok(group)
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
        Err(Error::refused(
            RefusalReason::ProposalsPending,
            format!(
                "{op} refused: {pending} membership proposal(s) are pending in this group, and a \
                 direct commit would silently absorb them (ADR-0045) — sweep them with \
                 commit_pending_proposals / stage_pending_proposals instead"
            ),
        ))
    }

    /// Refuse a role-table rewrite while another one is already pending
    /// (ADR-0045 § *What the client sprints found*, item 5).
    ///
    /// A role change is a read-modify-write whose read is the **committed**
    /// role table and whose write is now a proposal. On the direct path the
    /// caller's per-group lock sufficed: the first toggle's commit updated the
    /// table and the second read the updated one. A proposal changes nothing
    /// until it is swept, so two toggles taken a moment apart both read the
    /// **same** committed table and each proposes a table carrying only its own
    /// change. Serializing cannot repair that — there is nothing for the second
    /// read to observe — and neither can the caller, because
    /// [`ProposalKind::RoleChange`] carries no payload, so it cannot read back
    /// what it proposed.
    ///
    /// What the two then do is worse than losing one of them, and worse than
    /// ADR-0045 recorded. RFC 9420 admits **at most one**
    /// `GroupContextExtensions` proposal per commit, and OpenMLS enforces that
    /// as a hard validation failure rather than a silent selection
    /// (`validate_group_context_extensions_proposal` → `TooManyGCEProposals`,
    /// reached from `CommitBuilder::build`). So a pending set holding two role
    /// changes cannot be swept **at all**: every candidate committer's sweep
    /// errors, the store only ever empties when a commit is *merged*, and every
    /// direct-commit escape is closed by [`Self::refuse_if_proposals_pending`].
    /// A group whose every committer holds both is wedged for membership until
    /// a committer that holds only one lands a commit — and if none exists, it
    /// stays wedged. Chat is unaffected (see
    /// [`Self::create_application_message`], which parks the pending set).
    ///
    /// **Only a role change collides.** A pending add or remove composes into
    /// the same sweep perfectly well; the exclusive slot is the
    /// `GroupContextExtensions` one, so this guard is deliberately narrower
    /// than [`Self::refuse_if_proposals_pending`] and must stay that way.
    ///
    /// This is the *local* half. The sweep carries the other half — it keeps at
    /// most one role change (see `sweep_keeps`), so a set that reaches two
    /// through some path this guard cannot see still converges instead of
    /// wedging.
    fn refuse_if_role_change_pending(&self, op: &str) -> Result<()> {
        let pending = self
            .inner
            .pending_proposals()
            .filter(|qp| matches!(classify(qp.proposal()), ProposalKind::RoleChange))
            .count();
        if pending == 0 {
            return Ok(());
        }
        Err(Error::refused(
            RefusalReason::RoleChangePending,
            format!(
                "{op} refused: {pending} role-table change(s) are already pending in this group, \
                 and RFC 9420 admits at most one GroupContextExtensions proposal per commit, so a \
                 second one could not be swept (ADR-0045) — wait for the pending change to be \
                 committed, then propose again"
            ),
        ))
    }

    /// Add a member by their published [`KeyPackage`].
    ///
    /// Consumes one of the new member's published KeyPackages on success.
    /// The local group state is advanced to the new epoch atomically.
    /// One KeyPackage; see [`Self::add_members`] for several leaves in one
    /// commit.
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
        self.add_members(identity, vec![key_package])
    }

    /// Add several members in ONE commit with ONE Welcome (ADR-0050).
    ///
    /// A person is added as all of their devices, each its own leaf, and a
    /// group founder also admits its own other devices. Those leaves must
    /// land in a single commit: with one direct commit per leaf, every leaf
    /// but the last would hold a Welcome for an epoch the founder then
    /// advanced past, and — the founder being the only member at that point,
    /// with nobody to broadcast the intermediate commits to — would be
    /// stranded at its Welcome epoch forever. One commit gives every added
    /// leaf a Welcome for the same epoch; the returned Welcome carries the
    /// secrets of every added leaf and is delivered, byte-identical, to each
    /// of their mailboxes.
    ///
    /// Every leaf is checked for a device binding that resolves to an
    /// account, as [`Self::add_member`] does. An empty list is refused.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for an empty list, when a member proposal is
    /// pending, or for any OpenMLS-side failure; a typed
    /// [`RefusalReason::DeviceBindingInvalid`] refusal when any leaf's binding
    /// does not verify.
    pub fn add_members(
        &mut self,
        identity: &Identity,
        key_packages: Vec<KeyPackage>,
    ) -> Result<MemberAddition> {
        if key_packages.is_empty() {
            return Err(Error::Group("add_members: no KeyPackage given".to_owned()));
        }
        self.refuse_if_proposals_pending("add_member")?;
        // ADR-0050: the direct path admits nothing the proposal path would
        // refuse — a leaf whose credential does not resolve to an account.
        let mut added_accounts = Vec::with_capacity(key_packages.len());
        for key_package in &key_packages {
            let leaf = key_package.leaf_node();
            let Some(account) = crate::device_binding::account_key_of_credential(
                leaf.credential().serialized_content(),
                leaf.signature_key().as_slice(),
            ) else {
                return Err(Error::refused(
                    RefusalReason::DeviceBindingInvalid,
                    "add_member refused: the added leaf's device binding does not verify",
                ));
            };
            added_accounts.push(Some(account.to_vec()));
        }
        // ADR-0060: the local courtesy half of the one-to-one rule, before the
        // commit is built. It is also the check that admits the founding
        // commit of a one-to-one: the founder alone, plus one other person
        // with any number of devices, plus the founder's own other devices.
        one_to_one_holds(
            &CommitEffects::direct(Vec::new(), added_accounts),
            &self.members(),
            self.group_kind(),
        )?;
        let (commit, welcome, _group_info) = self
            .inner
            .add_members(&identity.provider, &identity.signature_keys, &key_packages)
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

    /// Add several members from their wire-format KeyPackages in one commit.
    ///
    /// The multi-leaf form of [`Self::add_member_from_wire`]; see
    /// [`Self::add_members`] for why a person's devices must share a commit.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyPackage`] for a validation failure of any package,
    /// else what [`Self::add_members`] returns.
    pub fn add_members_from_wire(
        &mut self,
        identity: &Identity,
        key_packages: &[&[u8]],
    ) -> Result<MemberAddition> {
        let mut validated = Vec::with_capacity(key_packages.len());
        for bytes in key_packages {
            let kp_in = crate::key_package::deserialize(bytes)?;
            validated.push(
                kp_in
                    .validate(identity.provider.crypto(), ProtocolVersion::Mls10)
                    .map_err(|e| Error::KeyPackage(format!("validate: {e:?}")))?,
            );
        }
        self.add_members(identity, validated)
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
        // ADR-0060: removing a person's last leaf from a one-to-one is refused
        // here, locally, before the commit is built; every peer refuses it too.
        one_to_one_holds(
            &CommitEffects::direct(vec![leaf.u32()], Vec::new()),
            &self.members(),
            self.group_kind(),
        )?;
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
        // ADR-0060: a one-to-one's GroupContext is frozen from creation, and no
        // rewrite anywhere may change a conversation's kind. Both are refused
        // on every peer; refusing here keeps this device from merging a commit
        // nobody else will.
        if self.group_kind() == Some(GroupKind::OneToOne) {
            return Err(Error::refused(
                RefusalReason::OneToOneHasNoRoles,
                "set_group_context_extensions refused: a one-to-one's GroupContext never changes",
            ));
        }
        if !self.kind_state().preserved_by(&KindState::of(&extensions)) {
            return Err(Error::refused(
                RefusalReason::GroupKindChanged,
                "set_group_context_extensions refused: it would change this conversation's kind",
            ));
        }
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

    /// The conversation's kind, read from the `0xFF03` GroupContext extension
    /// the creator wrote ([ADR-0060]). The same answer on every device of every
    /// member, from the moment it joins, and it never changes.
    ///
    /// `None` only for a group that carries no kind at all — a
    /// `PublisherGroup`, a `DeviceGroup`, or any other
    /// [`Group::create_with_id`] group, none of which is a conversation. A
    /// conversation without a readable kind is never joined
    /// ([`Group::join_from_welcome`]) and no commit may remove or change it.
    ///
    /// [ADR-0060]: ../../../docs/adr/0060-conversation-kind.md
    #[must_use]
    pub fn group_kind(&self) -> Option<GroupKind> {
        self.kind_state().kind()
    }

    /// The pre-merge group-kind facts the ADR-0060 gates compare a rewrite
    /// against.
    fn kind_state(&self) -> KindState {
        KindState::of(self.inner.extensions())
    }

    /// The GroupContext extension list a role-table rewrite installs: the
    /// current list **verbatim**, with only the `0xFF02` body replaced by
    /// `roles` (ADR-0060 section 4).
    ///
    /// **The one place a rewrite's extension list is built**, used by
    /// [`Group::set_group_roles`], [`Group::propose_group_roles`] and — through
    /// them — both ownership-transfer paths, so the four cannot drift. A
    /// `GroupContextExtensions` proposal replaces the WHOLE list, so anything a
    /// rewrite fails to restate is deleted: before this helper each site
    /// restated the two extensions it knew about, which would have dropped the
    /// `0xFF03` kind on the first promotion and been refused by every peer.
    /// Copying everything but the role table keeps the kind, the
    /// `RequiredCapabilities` naming it, and any extension added later.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] if the rebuilt list is not a valid extension set.
    fn role_rewrite_extensions(&self, roles: &RoleTable) -> Result<Extensions<GroupContext>> {
        let role = Extension::Unknown(ROLE_EXTENSION_TYPE, UnknownExtension(roles.encode()));
        let mut list = Vec::new();
        let mut placed = false;
        let mut has_required = false;
        for ext in self.inner.extensions().iter() {
            match ext {
                Extension::Unknown(t, _) if *t == ROLE_EXTENSION_TYPE => {
                    list.push(role.clone());
                    placed = true;
                },
                other => {
                    has_required |= matches!(other, Extension::RequiredCapabilities(_));
                    list.push(other.clone());
                },
            }
        }
        if !placed {
            list.push(role);
        }
        // A role table must stay in `required_capabilities` (OpenMLS refuses an
        // extension a member's leaf does not advertise). Every conversation
        // carries one from creation; this only covers a list that somehow
        // lacks it.
        if !has_required {
            list.push(role_required_capabilities());
        }
        Extensions::from_vec(list)
            .map_err(|e| Error::Group(format!("build role extensions: {e:?}")))
    }

    /// This member's role in the group, or `None` when the group has no role
    /// table (open group). Resolved by matching our own account-level signature
    /// key against the table — the single-account-leaf model ADR-0022 v1 assumes
    /// (Finding 1). A member not named in the table is [`Role::Member`].
    #[must_use]
    pub fn own_role(&self, identity: &Identity) -> Option<Role> {
        let account = identity.account_public_key()?;
        self.group_roles().map(|table| table.role_of(&account))
    }

    /// The account key the leaf at `leaf` resolves to (ADR-0050), or `None`
    /// for a leaf that is not in the roster or whose credential did not
    /// resolve.
    #[must_use]
    pub fn account_key_at(&self, leaf: u32) -> Option<Vec<u8>> {
        account_key_at_leaf(&self.members(), leaf).map(|k| k.to_vec())
    }

    /// Whether this device is its account's **acting** leaf in the group
    /// (ADR-0050): the one with the lexicographically lowest leaf signature
    /// key among the account's leaves. Only that leaf sweeps, so two devices of
    /// one admin never both act. A device that is not in the roster is not
    /// acting.
    fn is_accounts_acting_leaf(&self, identity: &Identity) -> bool {
        let Some(account) = identity.account_public_key() else {
            return false;
        };
        let own_key = identity.signature_public_key();
        let members = self.members();
        if !members.iter().any(|m| m.signature_public_key == own_key) {
            return false;
        }
        !members.iter().any(|m| {
            m.account_key.as_slice() == account && m.signature_public_key.as_slice() < own_key
        })
    }

    /// Retain a non-member's external-join proposal for an owner/admin decision
    /// (ADR-0027, G-3). Validates it is genuinely an `Add` from a
    /// `NewMemberProposal` sender (defence in depth — a member must not smuggle
    /// a different proposal through this path), extracts the joiner's signature
    /// key + KeyPackage, de-dups by signature key, and retains it under the
    /// [`MAX_PENDING_EXTERNAL_JOINS`] cap. The event is always surfaced even
    /// when the cap drops the retention.
    fn retain_external_join(
        &mut self,
        identity: &Identity,
        qp: &QueuedProposal,
    ) -> Result<Incoming> {
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
        let joiner_leaf = key_package.leaf_node();
        let joiner_signature_key = joiner_leaf.signature_key().as_slice().to_vec();
        // ADR-0050: a leaf whose credential does not resolve to an account is
        // never retained — surfaced, like a full quarantine, as `retained:
        // false`, since a stranger must not be able to fail a member's loop.
        let binding_resolves = crate::device_binding::account_key_of_credential(
            joiner_leaf.credential().serialized_content(),
            &joiner_signature_key,
        )
        .is_some();

        // ADR-0045 S7 — reap first, so a request whose joiner has since landed
        // frees the slot it was occupying instead of holding it to the cap.
        self.reap_joined_external_joins();

        let already_pending = self
            .pending_external_joins
            .iter()
            .any(|p| p.joiner_signature_key == joiner_signature_key);
        let already_a_member = self.joiner_is_already_a_member(&joiner_signature_key);
        // ADR-0060: nobody joins a one-to-one from outside. The knock is
        // consumed and surfaced as `retained: false` — the shape a full
        // quarantine already has — so nothing is ever offered for approval.
        let one_to_one = self.group_kind() == Some(GroupKind::OneToOne);
        if binding_resolves
            && !one_to_one
            && !already_pending
            && !already_a_member
            && self.pending_external_joins.len() < MAX_PENDING_EXTERNAL_JOINS
        {
            self.pending_external_joins.push(PendingExternalJoin {
                joiner_signature_key: joiner_signature_key.clone(),
                key_package,
            });
        }
        self.persist_pending_external_joins(identity)?;
        // Report whether an approve can act on this, rather than leaving the
        // caller to find out by failing. `already_pending` counts as retained:
        // the entry is there, this event is just a duplicate of one already
        // seen. `already_a_member` and a full quarantine do not.
        let retained = self
            .pending_external_joins
            .iter()
            .any(|p| p.joiner_signature_key == joiner_signature_key);
        Ok(Incoming::ExternalJoinProposal {
            joiner_signature_key,
            retained,
        })
    }

    /// `true` when `signature_key` already sits in this group's roster.
    fn joiner_is_already_a_member(&self, signature_key: &[u8]) -> bool {
        self.members()
            .iter()
            .any(|m| m.signature_public_key.as_slice() == signature_key)
    }

    /// Drop every retained external-join request whose joiner has since become
    /// a member (ADR-0045 S7).
    ///
    /// **Why this became necessary.** Under ADR-0027 an approve added the
    /// joiner immediately, so the only device that ever held a stale request
    /// was one whose own approve had just consumed it. S7 splits the decision
    /// from the epoch advance: admin A approves, some other committer sweeps,
    /// and the joiner lands as a member on **every** device — including admin
    /// B, whose own retained copy of the same request (it received the same
    /// external proposal) nothing has ever removed, because
    /// [`Self::reject_external_join`] is local-only and no message says "this
    /// one is settled".
    ///
    /// Membership is that message. The roster is authenticated group state
    /// every member observes identically, so "is this joiner in the group" is a
    /// question every device answers the same way and at the same epoch, with
    /// no new wire artifact and nothing to authorise.
    ///
    /// Approving a stale request would not be unsafe — it is an ordinary
    /// authorised `Add` that `sweep_keeps` clause 3 excludes for colliding with
    /// a staying member — but it would occupy a slot in every member's pending
    /// set until the next sweep, and it would show the admin a request that is
    /// no longer a request.
    fn reap_joined_external_joins(&mut self) {
        let members = self.members();
        self.pending_external_joins.retain(|p| {
            !members
                .iter()
                .any(|m| m.signature_public_key == p.joiner_signature_key)
        });
    }

    /// The signature key of every external-join proposal currently awaiting a
    /// decision (ADR-0027). Each is the handle for
    /// [`approve_external_join`](Self::approve_external_join) /
    /// [`reject_external_join`](Self::reject_external_join).
    ///
    /// A joiner that has since become a member is **never** listed, whichever
    /// device's sweep admitted them — see [`Self::reap_joined_external_joins`].
    /// The filter, rather than the reap, is what makes that a guarantee: it is
    /// the only read, so it cannot be reached by a path that forgot to reap.
    #[must_use]
    pub fn pending_external_joins(&self) -> Vec<Vec<u8>> {
        let members = self.members();
        self.pending_external_joins
            .iter()
            .filter(|p| {
                !members
                    .iter()
                    .any(|m| m.signature_public_key == p.joiner_signature_key)
            })
            .map(|p| p.joiner_signature_key.clone())
            .collect()
    }

    /// The **account** key behind a pending external-join request, resolved
    /// from the retained KeyPackage's ADR-0050 device binding (verified: the
    /// binding is signed by the account key it names), or the joiner's own key
    /// for an unbound Phase 1 credential. `None` when no such request is
    /// pending or the credential carries a binding that does not verify.
    ///
    /// A request is keyed on the joiner's device leaf key, which names one of
    /// a person's phones; contacts are stored under the account key. Without
    /// this, an admin's pending list could only ever say "Member", even for a
    /// joiner the admin knows. Read-only, and only a label: approving still
    /// goes by the leaf key and through the role gate.
    #[must_use]
    pub fn pending_external_join_account_key(
        &self,
        joiner_signature_key: &[u8],
    ) -> Option<[u8; 32]> {
        let pending = self
            .pending_external_joins
            .iter()
            .find(|p| p.joiner_signature_key == joiner_signature_key)?;
        let leaf = pending.key_package.leaf_node();
        crate::device_binding::account_key_of_credential(
            leaf.credential().serialized_content(),
            leaf.signature_key().as_slice(),
        )
    }

    /// The retained KeyPackage for `proposal_ref`, cloned so the caller can
    /// attempt an approve without consuming the request.
    ///
    /// **The request survives a failed approve.** Both approve paths look up
    /// here, act, and only remove the entry once the act succeeded — so a
    /// refusal (wrong role, a full pending set, an OpenMLS-side failure) leaves
    /// the admin able to retry rather than silently discarding a stranger's
    /// request nobody can get back.
    fn pending_external_join_key_package(&self, proposal_ref: &[u8]) -> Result<KeyPackage> {
        if self.joiner_is_already_a_member(proposal_ref) {
            return Err(Error::refused(
                RefusalReason::JoinerAlreadyAMember,
                "external join approve refused: that joiner is already a member of this group",
            ));
        }
        self.pending_external_joins
            .iter()
            .find(|p| p.joiner_signature_key == proposal_ref)
            .map(|p| p.key_package.clone())
            .ok_or_else(|| {
                Error::refused(
                    RefusalReason::NoSuchJoinRequest,
                    "no pending external join for that key",
                )
            })
    }

    /// Discard the retained request for `proposal_ref`. Idempotent.
    fn drop_pending_external_join(&mut self, proposal_ref: &[u8]) {
        self.pending_external_joins
            .retain(|p| p.joiner_signature_key != proposal_ref);
    }

    /// Write the retained requests to their provider row, or delete the row
    /// when nothing is retained, so a group with no pending request leaves no
    /// trace in the snapshot.
    ///
    /// Called after every mutation of the set — retain, approve, reject — and
    /// only then: the reads never write. A failure here is surfaced rather
    /// than swallowed, because a mutation the snapshot will not carry is the
    /// exact defect this row exists to close.
    ///
    /// # Errors
    ///
    /// [`Error::Identity`] if the provider lock is poisoned; what
    /// [`encode_pending_external_joins`] returns.
    fn persist_pending_external_joins(&self, identity: &Identity) -> Result<()> {
        let key = pending_external_joins_row_key(&self.group_id());
        let mut map = identity
            .provider
            .storage()
            .values
            .write()
            .map_err(|_| Error::Identity("storage RwLock poisoned".to_string()))?;
        if self.pending_external_joins.is_empty() {
            map.remove(&key);
        } else {
            map.insert(
                key,
                encode_pending_external_joins(&self.pending_external_joins)?,
            );
        }
        Ok(())
    }

    /// Read `group_id`'s retained requests back from the provider row.
    ///
    /// An absent row is an empty set. A row that does not decode — written
    /// by no version of this crate, or damaged in the store — is **deleted
    /// and read as empty**, never as an error: this is local bookkeeping
    /// that a joiner can always refresh by asking again, and failing the
    /// whole `Group::load` over it would take the chat down with it. The
    /// same outcome the in-memory v1 produced on every restart, confined to
    /// the corrupt case.
    fn load_pending_external_joins(
        identity: &Identity,
        group_id: &[u8],
    ) -> Vec<PendingExternalJoin> {
        let key = pending_external_joins_row_key(group_id);
        let Ok(mut map) = identity.provider.storage().values.write() else {
            return Vec::new();
        };
        let Some(row) = map.get(&key) else {
            return Vec::new();
        };
        if let Ok(pending) = decode_pending_external_joins(row, identity) {
            return pending;
        }
        map.remove(&key);
        Vec::new()
    }

    /// Approve a retained external-join request — **proposes** adding the
    /// joiner (ADR-0027 G-3, converged onto the ADR-0045 proposal path by S7).
    /// `proposal_ref` is the joiner's signature key from
    /// [`Incoming::ExternalJoinProposal`] / [`pending_external_joins`](Self::pending_external_joins).
    ///
    /// Returns the wire-encoded **Proposal** to publish to the group journal.
    /// The epoch does not advance and there is no Welcome yet: the Welcome is
    /// produced by whichever device sweeps this into a commit
    /// ([`Self::commit_pending_proposals`] / [`Self::stage_pending_proposals`]).
    ///
    /// # What the approval *is*, after S7
    ///
    /// The admin's decision is expressed as an **`Add` proposal the admin
    /// itself authors**, carrying the joiner's retained KeyPackage. The
    /// joiner's own external proposal is still never stored and never
    /// committed — the same structural property ADR-0027 rested on — so a
    /// stranger holding a `/g/` link may still only *ask*.
    ///
    /// That indistinguishability from an ordinary admin add is the point, and
    /// it is what makes S7 possible at all. ADR-0045 decision 4's own
    /// recommendation was the opposite — store the stranger's proposal in the
    /// MLS store and give the sweep filter a second arm admitting it once an
    /// admin has "marked it approved" — and that does not survive contact with
    /// the rest of the design, three ways. The mark is device-local, so a
    /// **different** admin's sweep could not see it; every peer's commit gate
    /// would refuse the resulting commit, because a `NewMemberProposal` sender
    /// resolves to no member and [`crate::group_proposals::authorise`] refuses
    /// every kind in that case, and the only evidence available to relax that
    /// is "an admin committed it" — which is precisely the auto-admit ADR-0045
    /// forbids; and merging the two stores would let any stranger with a link
    /// consume the member pending set, which is a denial-of-membership
    /// primitive handed to non-members. Authoring the `Add` puts the approval
    /// on the wire as an MLS-authenticated artifact instead, judged by the one
    /// existing rule, with no new authorisation channel.
    ///
    /// **Role-gated** exactly as before, and by the same rule as every other
    /// add: [`crate::group_proposals::authorise`] admits an `Add` from an owner
    /// or admin only, enforced locally here and again by every peer's receipt
    /// gate and commit gate. A refused approve produces no proposal and leaves
    /// the request pending (fail-closed, invariant #4).
    ///
    /// **A pending membership proposal no longer blocks an approve.** It used
    /// to — `refuse_if_proposals_pending` guarded the direct commit this
    /// replaces — and under ADR-0045 a pending proposal is the normal state, so
    /// an admin deferring to a live owner could essentially never approve.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group carries no role table (it has no
    /// committer, so a proposal published into it would never be swept — use
    /// [`crate::PublisherGroup::approve_external_join`], the only such group
    /// that receives external-join requests), when this device's role does not
    /// authorise an add, when the pending set is full, when no retained request
    /// matches `proposal_ref`, when that joiner is already a member, or on any
    /// OpenMLS-side failure.
    pub fn approve_external_join(
        &mut self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<Vec<u8>> {
        self.role_table_or_refuse("external join approve")?;
        if self.group_kind() == Some(GroupKind::OneToOne) {
            return Err(Error::refused(
                RefusalReason::OneToOneMembershipFixed,
                "external join approve refused: a one-to-one always has the same two people",
            ));
        }
        let key_package = self.pending_external_join_key_package(proposal_ref)?;
        // Preserve the typed reason rather than flattening it into prose.
        //
        // This used to be an unconditional `Error::Group(format!(…))`, which
        // destroyed the `RefusalReason` `propose_add_member` had just produced —
        // so an approve refused by a full pending set reached every client with
        // no reason at all, and could therefore never participate in the
        // retry-when-the-queue-drains rule even though it is exactly the case
        // that rule exists for. The wrap stays for a refusal that carries no
        // reason, because naming the operation is what makes those readable.
        let proposal = self
            .propose_add_member(identity, key_package)
            .map_err(|e| {
                if e.refusal_reason().is_some() {
                    e
                } else {
                    Error::Group(format!("external join approve refused: {e}"))
                }
            })?;
        // Only now. The decision has been made and is on its way to the wire;
        // a refusal above left the request where it was.
        self.drop_pending_external_join(proposal_ref);
        self.persist_pending_external_joins(identity)?;
        Ok(proposal)
    }

    /// Approve a retained external-join request as a **direct commit**, for a
    /// group that has no role table and therefore no committer to sweep a
    /// proposal (ADR-0045 S7).
    ///
    /// `pub(crate)` and reachable from exactly one place —
    /// [`crate::PublisherGroup::approve_external_join`] — so the direct route
    /// exists only where the proposal route cannot. It is not a fallback: it
    /// **refuses** a group that carries a role table rather than quietly
    /// direct-committing into one that has a committer, which would be the
    /// second ungated route into membership ADR-0045 names as the hazard
    /// rebuilt under a prudent name.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group carries a role table, when no retained
    /// request matches `proposal_ref`, when that joiner is already a member, or
    /// on any OpenMLS-side add failure.
    pub(crate) fn approve_external_join_directly(
        &mut self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<MemberAddition> {
        if self.group_roles().is_some() {
            return Err(Error::Group(
                "direct external join approve refused: this group carries a role table, so it \
                 has a committer — approve it as a proposal instead (ADR-0045 S7)"
                    .into(),
            ));
        }
        // A counted-zero no-op here (a table-less group's receipt gate stores
        // nothing), kept for the same reason every other direct path keeps it:
        // the guard belongs to the shape, not to the group.
        self.refuse_if_proposals_pending("external join approve")?;
        let key_package = self.pending_external_join_key_package(proposal_ref)?;
        let addition = self.add_member(identity, key_package)?;
        self.drop_pending_external_join(proposal_ref);
        self.persist_pending_external_joins(identity)?;
        Ok(addition)
    }

    /// Reject a retained external-join proposal — discards it locally without
    /// adding the joiner or advancing the epoch (ADR-0027). Local-only (no
    /// commit, no peer effect); the clients gate the affordance on `own_role`.
    /// `identity` is what the durable copy of the set is written through, so
    /// a rejected request stays rejected across a restart.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] if no pending proposal matches `proposal_ref`.
    pub fn reject_external_join(&mut self, identity: &Identity, proposal_ref: &[u8]) -> Result<()> {
        let pos = self
            .pending_external_joins
            .iter()
            .position(|p| p.joiner_signature_key == proposal_ref)
            .ok_or_else(|| {
                Error::refused(
                    RefusalReason::NoSuchJoinRequest,
                    "no pending external join for that key",
                )
            })?;
        self.pending_external_joins.remove(pos);
        self.persist_pending_external_joins(identity)
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
        self.refuse_role_change_in_one_to_one("role-table change")?;
        if !table.is_owner(identity.signature_public_key()) {
            return Err(Error::refused(
                RefusalReason::NotOwner,
                "role-table change refused: this device is not the group owner",
            ));
        }
        if !self
            .members()
            .iter()
            .any(|m| m.signature_public_key.as_slice() == roles.owner().as_slice())
        {
            return Err(Error::refused(
                RefusalReason::OwnerNotAMember,
                "role-table change refused: the named owner is not a member of this group",
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
        // the list is the current one verbatim with only the role table swapped
        // (ADR-0060 — the kind and `required_capabilities` ride through).
        let exts = self.role_rewrite_extensions(roles)?;
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
            Error::refused(
                RefusalReason::NoRoleTable,
                format!("{op} refused: this group carries no role table, so it has no committer"),
            )
        })
    }

    /// Refuse any role-table rewrite in a one-to-one (ADR-0060 section 5): it
    /// has no roles to change, and its GroupContext is frozen from creation.
    fn refuse_role_change_in_one_to_one(&self, op: &str) -> Result<()> {
        if self.group_kind() == Some(GroupKind::OneToOne) {
            return Err(Error::refused(
                RefusalReason::OneToOneHasNoRoles,
                format!("{op} refused: a one-to-one has no roles"),
            ));
        }
        Ok(())
    }

    /// Run the ADR-0045 rule locally for something *this* device is about to
    /// propose. A courtesy check that keeps our own store clean and gives the
    /// user an immediate error; the enforcing copies are the peers' receipt gate
    /// and commit gate, which do not trust us.
    ///
    /// It also applies the pending-set cap. That one is not a courtesy: an
    /// honest device must not hand its peers a proposal their receipt gates
    /// would refuse for being over the cap, because it would then hold a
    /// proposal nobody else does and sweep a commit they cannot resolve. The cap
    /// is computed by the same [`pending_capacity`] on both sides for exactly
    /// that reason — including the owner reserve, which is what keeps a full set
    /// from taking the owner's removal and role change away
    /// ([`MAX_RESERVED_OWNER_RESPONSES`]).
    ///
    /// The cap is still checked **before** [`authorise`], so which error a
    /// caller sees is unchanged; the reserve only widens the capacity for a
    /// proposal `authorise` was going to accept anyway.
    fn authorise_own_proposal(&self, identity: &Identity, kind: &ProposalKind) -> Result<()> {
        let table = self.role_table_or_refuse("proposal")?;
        let members = self.members();
        let proposer = Proposer {
            account: identity.account_public_key().ok_or_else(|| {
                Error::Group("proposal refused: identity signature key is not 32 bytes".into())
            })?,
        };
        // ADR-0060 first: in a one-to-one a request that changes its people or
        // its roles is not permitted at all, so saying "the queue is full" or
        // "you are not an admin" instead would invite a pointless retry.
        one_to_one_allows(self.group_kind(), &members, Some(proposer), kind)?;
        self.refuse_if_device_limit_reached(&members, kind)?;
        let capacity = pending_capacity(is_owner_response(&table, Some(proposer), kind));
        if self.inner.pending_proposals().count() >= capacity {
            return Err(Error::refused(
                RefusalReason::PendingSetFull,
                format!(
                    "proposal refused: this group already holds {capacity} pending proposals — a \
                     committer must sweep them first"
                ),
            ));
        }
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
        let leaf = key_package.leaf_node();
        let signature_key = leaf.signature_key().as_slice().to_vec();
        let kind = ProposalKind::Add {
            account_key: crate::device_binding::account_key_of_credential(
                leaf.credential().serialized_content(),
                &signature_key,
            )
            .map(|k| k.to_vec()),
            signature_key,
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
    /// **One at a time.** Only one role change may be pending at once — see
    /// [`Self::refuse_if_role_change_pending`] for why, and note that the guard
    /// is deliberately narrow: a pending add or remove does not block a role
    /// change, only another role change does. A caller that wants to pre-empt
    /// the refusal rather than surface it reads
    /// [`Self::pending_proposals`] and looks for a
    /// [`ProposalKind::RoleChange`]; that is the same question this asks, and
    /// it needs no new surface.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the group has no role table, when this device is
    /// not the owner, when the new table names an owner who is not a member of
    /// the group, when a role-table change is already pending, or on any
    /// OpenMLS-side failure.
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
        // Last, so a caller handed an unenforceable table learns *that* even
        // while another change is in flight: authority and validity first, then
        // availability. `set_group_roles` needs no equivalent — its
        // `refuse_if_proposals_pending` already refuses on any pending
        // proposal, of which a role change is one.
        self.refuse_if_role_change_pending("role-table change")?;
        // As in `set_group_roles`: a GroupContextExtensions proposal replaces
        // the WHOLE extension set, so it is the current list verbatim with only
        // the role table swapped (ADR-0060).
        let exts = self.role_rewrite_extensions(roles)?;
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
        self.refuse_role_change_in_one_to_one("ownership transfer")?;
        let own_account = identity.account_public_key().unwrap_or_default();
        if !table.is_owner(&own_account) {
            return Err(Error::refused(
                RefusalReason::NotOwner,
                "ownership transfer refused: this device is not the group owner",
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
        if !self.members().iter().any(|m| m.account_key == new_owner) {
            return Err(Error::refused(
                RefusalReason::OwnerNotAMember,
                "ownership transfer refused: the named owner is not a member of this group",
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
    /// Being a role change is also why it collides with one, **in both
    /// directions**: a transfer occupies the same exclusive
    /// `GroupContextExtensions` slot as a promote or demote, so
    /// [`Self::refuse_if_role_change_pending`] refuses a transfer while a role
    /// change is pending and refuses a role change while a transfer is. That
    /// falls out of delegating to [`Group::propose_group_roles`] rather than
    /// being restated here, which is the point: one rule, one place.
    ///
    /// # Errors
    ///
    /// As [`Group::transfer_ownership`], plus [`Error::Group`] when a
    /// role-table change — including another transfer — is already pending.
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
        // ADR-0050: rank the ACCOUNT, and let only the account's acting leaf
        // hold it, so two devices of one admin never both sweep.
        if !self.is_accounts_acting_leaf(identity) {
            return None;
        }
        let account = identity.account_public_key()?;
        if let Some(rank) = crate::group_roles::committer_rank(&table, &account) {
            return Some(rank);
        }
        // ADR-0050 section 2.5: a pending set that holds only this account's
        // own self-service proposals may be swept by this account, ranked
        // right after the last admin, so a second device joins a chat without
        // waiting for the other person to come online.
        if self.pending_is_self_service_of(&account) {
            return u32::try_from(table.admins().len()).ok()?.checked_add(1);
        }
        None
    }

    /// A **peer's** ADR-0045 committer rank, from this device's view: the rank
    /// of the account the leaf with `leaf_signature_key` belongs to, held only
    /// by that account's acting leaf (ADR-0050 — the lowest leaf signature key
    /// among the account's leaves). `None` for a plain member, a leaf not in
    /// the roster, a non-acting leaf of a ranked account, or a group with no
    /// role table.
    ///
    /// Rank the key when you take the verdict, not when a beacon arrived: a
    /// rank is a position in the role table's canonical order and renumbers
    /// on every role change.
    #[must_use]
    pub fn committer_rank_of(&self, leaf_signature_key: &[u8]) -> Option<u32> {
        let table = self.group_roles()?;
        let members = self.members();
        let member = members
            .iter()
            .find(|m| m.signature_public_key.as_slice() == leaf_signature_key)?;
        let account: [u8; 32] = member.account_key.as_slice().try_into().ok()?;
        let acting = !members.iter().any(|m| {
            m.account_key.as_slice() == account
                && m.signature_public_key.as_slice() < leaf_signature_key
        });
        if !acting {
            return None;
        }
        crate::group_roles::committer_rank(&table, &account)
    }

    /// `true` when the pending set is non-empty and every proposal in it is a
    /// self-service proposal of `account` (ADR-0050): an `Add` of a leaf bound
    /// to `account`, a `Remove` of one of `account`'s leaves, or a `Leave`
    /// proposed by one of them.
    fn pending_is_self_service_of(&self, account: &[u8; 32]) -> bool {
        let members = self.members();
        let mut any = false;
        for qp in self.inner.pending_proposals() {
            any = true;
            let sender_account = match qp.sender() {
                Sender::Member(idx) => account_key_at_leaf(&members, idx.u32()),
                _ => None,
            };
            let own = match classify(qp.proposal()) {
                ProposalKind::Add { account_key, .. } => {
                    account_key.as_deref() == Some(account.as_slice())
                },
                ProposalKind::Remove { target_leaf } => {
                    account_key_at_leaf(&members, target_leaf) == Some(*account)
                },
                ProposalKind::Leave => sender_account == Some(*account),
                _ => false,
            };
            if !own {
                return false;
            }
        }
        any
    }

    /// Refuse an `Add` that would take its account past
    /// [`MAX_LEAVES_PER_ACCOUNT`] in this group (ADR-0050). Counted over the
    /// current roster plus the pending `Add`s of the same account, so N devices
    /// proposed in one round cannot overshoot together.
    fn refuse_if_device_limit_reached(
        &self,
        members: &[MemberRecord],
        kind: &ProposalKind,
    ) -> Result<()> {
        let ProposalKind::Add {
            account_key: Some(account),
            ..
        } = kind
        else {
            return Ok(());
        };
        // Distinct pending device keys of that account: N proposals naming one
        // device are one device (the sweep admits each key once).
        let mut pending_keys: Vec<Vec<u8>> = Vec::new();
        for qp in self.inner.pending_proposals() {
            if let ProposalKind::Add {
                account_key: Some(a),
                signature_key,
            } = classify(qp.proposal())
            {
                if &a == account
                    && !pending_keys.contains(&signature_key)
                    && !members
                        .iter()
                        .any(|m| m.signature_public_key == signature_key)
                {
                    pending_keys.push(signature_key);
                }
            }
        }
        if leaves_of_account(members, account) + pending_keys.len() >= MAX_LEAVES_PER_ACCOUNT {
            return Err(Error::refused(
                RefusalReason::DeviceLimitReached,
                format!(
                    "add proposal refused: that account already holds                      {MAX_LEAVES_PER_ACCOUNT} devices in this group"
                ),
            ));
        }
        Ok(())
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

    /// Snapshot everything [`added_leaf_can_join`] needs to decide whether a
    /// prospective leaf may live in **this** group at **this** epoch.
    ///
    /// Read into owned values before the commit builder borrows the group, so
    /// [`SweepContext`] can stay free of any reference into `self` — the same
    /// discipline the roster and role table already follow.
    ///
    /// `removed_leaves` is the set the sweep will remove, so the member-derived
    /// fields describe the roster the commit **produces**, matching how OpenMLS
    /// seeds its own key-uniqueness sets.
    fn admission_requirements(&self, removed_leaves: &[u32]) -> GroupAdmission {
        let extensions = self.inner.extensions();
        let required = extensions.required_capabilities();
        let staying = self
            .inner
            .members()
            .filter(|m| !removed_leaves.contains(&m.index.u32()));
        let mut staying_credential_types = Vec::new();
        let mut staying_encryption_keys = Vec::new();
        for m in staying {
            let credential_type = m.credential.credential_type();
            if !staying_credential_types.contains(&credential_type) {
                staying_credential_types.push(credential_type);
            }
            staying_encryption_keys.push(EncryptionKey::from(m.encryption_key));
        }
        GroupAdmission {
            ciphersuite: self.inner.ciphersuite(),
            protocol_version: GROUP_PROTOCOL_VERSION,
            group_extension_types: extensions.iter().map(Extension::extension_type).collect(),
            required_extension_types: required
                .map(|r| r.extension_types().to_vec())
                .unwrap_or_default(),
            required_proposal_types: required
                .map(|r| r.proposal_types().to_vec())
                .unwrap_or_default(),
            required_credential_types: required
                .map(|r| r.credential_types().to_vec())
                .unwrap_or_default(),
            staying_credential_types,
            staying_encryption_keys,
        }
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
            return Err(Error::refused(
                RefusalReason::SweepAlreadyStaged,
                "sweep refused: a staged sweep is already awaiting a verdict",
            ));
        }
        let table = self.role_table_or_refuse("sweep")?;
        // ADR-0050: authority is the ACCOUNT's, and a pending set made only of
        // this account's own self-service proposals may be swept by it
        // (section 2.5) — which is how a second device joins a chat without
        // waiting for the other person to come online.
        let own_account = identity.account_public_key().unwrap_or_default();
        if !table.can_administer(&own_account) && !self.pending_is_self_service_of(&own_account) {
            return Err(Error::refused(
                RefusalReason::NotACommitter,
                "sweep refused: this device is not owner or admin",
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
        // Neither can excluding a duplicate `Add` — see `sweep_keeps` clause 3.
        let effects = commit_effects(
            self.inner
                .pending_proposals()
                .filter(|qp| proposal_is_authorised(&table, &members, qp)),
        );
        let role_change_survives = owner_survives(&effects, &members, table.owner()).is_ok();
        // Removes and leaves are never excluded by the filter, so the authorised
        // set's removals are exactly the emitted commit's removals — which is
        // what makes this the right roster for the key-collision clause.
        let removed_leaves = effects.removed_leaves().to_vec();
        // Read the group's own admission requirements out BEFORE the commit
        // builder borrows the group: everything `sweep_keeps` consults must be
        // owned by this frame, which is why `SweepContext` holds no reference
        // into `self`.
        let admission = self.admission_requirements(&removed_leaves);
        let kind_before = self.kind_state();
        let ctx = SweepContext {
            table: &table,
            members: &members,
            kind_before: &kind_before,
            role_change_survives,
            removed_leaves: &removed_leaves,
            admission: &admission,
            identity,
        };

        // The enumeration's verdict, taken ONCE and recorded as the set of
        // proposal references it keeps. Everything downstream selects by
        // membership in that set rather than by re-running the filter, which is
        // what lets `Group::stage_selection` vary the selection when the
        // builder refuses it — and, as a free side effect, closes the expiry
        // straddle the old prediction-then-filter pair had (see
        // `added_leaf_can_join`), because the clock is now consulted once.
        let mut kept = SweepState::default();
        let candidates: Vec<ProposalRef> = self
            .inner
            .pending_proposals()
            .filter(|qp| sweep_keeps(&ctx, &mut kept, qp))
            .map(|qp| qp.proposal_reference_ref().clone())
            .collect();
        self.stage_selection(identity, candidates)
    }

    /// Turn a chosen set of pending proposals into one staged Commit, narrowing
    /// the set by exclusion if the builder refuses it.
    ///
    /// The two layers, in the order they run, because a later reader must not
    /// mistake either for a replacement of the other:
    ///
    /// 1. [`sweep_keeps`] — the **enumeration**, which has already produced
    ///    `candidates`. It is fast, it costs no build, and every exclusion it
    ///    makes has a name (a duplicate key, a leaf this group will not accept,
    ///    a second role change). It handles every cause anybody has identified,
    ///    and it stays.
    /// 2. This — the **last resort**, reached only when the builder refuses the
    ///    enumeration's verdict anyway. It has no name for what it excludes and
    ///    cannot acquire one, because `CreateCommitError` carries no
    ///    `ProposalRef`, no leaf index and no signature key on any variant.
    ///
    /// **Why 1 cannot be sufficient on its own.** The enumeration restates a
    /// rule set this crate does not own — openmls's `validate_*` family, itself
    /// tracking RFC 9420 — so it is a mirror, and a mirror can go out of date
    /// without anybody noticing. That is not a hypothetical worry: one of the
    /// conditions it already restates is a KeyPackage's **lifetime**, and
    /// ADR-0045 gives a pending request no deadline, so a proposal can sit until
    /// the KeyPackage it names expires and every sweep from then on fails. No
    /// adversary, no bug, no unusual usage — just time. A failure class that
    /// arrives on its own is a failure class that will arrive again in a shape
    /// the list does not have.
    ///
    /// **And an unbuildable sweep is not a lost round.** The proposal store
    /// empties only when a commit is *merged*, every direct commit is refused
    /// while proposals are pending ([`Group::refuse_if_proposals_pending`]), and
    /// nothing discards a proposal — so a sweep that cannot be built means no
    /// commit can be produced, which means the store can never be emptied, which
    /// means no commit can ever be produced. The group's membership is bricked
    /// permanently, and invisibly, because chat keeps working over the top of it
    /// ([`Group::create_application_message`] parks the pending set).
    ///
    /// # The search
    ///
    /// Additive: start from nothing, offer each candidate in store order, and
    /// keep it only if the selection still builds. Chosen over leave-one-out
    /// deliberately — leave-one-out is cheaper when there is exactly one
    /// offender, but needs a fresh round per offender and degrades to `O(n^2)`
    /// when there are several, which is precisely the case that cannot be ruled
    /// out when the cause is unknown. The additive shape costs one probe per
    /// candidate whether there is one offender or all of them, and it is two
    /// flat loops with no fixpoint, so termination is structural rather than
    /// argued.
    ///
    /// A second pass then re-offers each rejected candidate against the final
    /// selection, which is what makes the search insensitive to store order for
    /// the proposals that are only committable **together** — a `Remove` and the
    /// re-`Add` of the same signature key, or an ownership transfer and the
    /// `Add` that puts its named owner in the group.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when the narrowed selection fails to build after all
    /// (the clock can move under a KeyPackage lifetime between a probe and the
    /// real build; the next sweep re-runs the enumeration, which will exclude it
    /// by name), or on any OpenMLS-side staging failure.
    fn stage_selection(
        &mut self,
        identity: &Identity,
        candidates: Vec<ProposalRef>,
    ) -> Result<Option<ProposalCommit>> {
        // Nothing to sweep => emit no commit at all, rather than an empty one
        // that would advance the epoch (and race other committers) for no
        // effect.
        if candidates.is_empty() {
            return Ok(None);
        }
        // The commit-wide rules first (ADR-0060): the enumeration judges each
        // proposal alone, and two individually harmless ones — the two leaves
        // of one person in a one-to-one — can together build a commit every
        // peer refuses. A selection a peer would refuse is narrowed exactly as
        // one the builder refuses, never published.
        let admissible = {
            let members = self.members();
            let queued: Vec<QueuedProposal> = self.inner.pending_proposals().cloned().collect();
            self.selection_is_admissible(&queued, &candidates, &members)
        };
        let first = if admissible {
            self.stage_sweep_of(identity, &candidates)
        } else {
            Err(SweepAttemptError::Refused(Error::Group(
                "sweep: the selection breaks a commit-wide rule a peer would apply".into(),
            )))
        };
        match first {
            Ok(commit) => Ok(Some(commit)),
            // Anything that is not the builder refusing this selection is not
            // something a different selection can fix.
            Err(SweepAttemptError::Fatal(e)) => Err(e),
            Err(SweepAttemptError::Refused(refusal)) => {
                let members = self.members();
                let queued: Vec<QueuedProposal> = self.inner.pending_proposals().cloned().collect();
                let narrowed =
                    self.narrow_sweep_by_exclusion(identity, &queued, &members, &candidates);
                if narrowed.is_empty() {
                    // Every candidate is uncommittable. The honest outcome is
                    // the same one the enumeration produces when its filter
                    // keeps nothing: no commit at all.
                    return Ok(None);
                }
                if narrowed.len() == candidates.len() {
                    // The search accepts a candidate only when the selection
                    // including it built, so accepting every one of them means
                    // the whole set built — contradicting the refusal that sent
                    // us here, unless the clock moved under a KeyPackage
                    // lifetime in between. Surface the original refusal rather
                    // than re-attempt a set that has already been refused once:
                    // there is no narrowing left to try, and the next sweep
                    // re-runs the enumeration, which will exclude the expired
                    // proposal by name.
                    return Err(refusal);
                }
                self.stage_sweep_of(identity, &narrowed)
                    .map(Some)
                    .map_err(SweepAttemptError::into_error)
            },
        }
    }

    /// Build and stage one Commit carrying exactly the proposals in
    /// `selection`.
    ///
    /// Selection is by [`ProposalRef`] membership rather than by re-running
    /// [`sweep_keeps`], so the same call serves the enumeration's verdict and
    /// any narrowing of it, and so the filter is a pure function of `selection`
    /// — two builds over one selection can never disagree.
    fn stage_sweep_of(
        &mut self,
        identity: &Identity,
        selection: &[ProposalRef],
    ) -> std::result::Result<ProposalCommit, SweepAttemptError> {
        count_sweep_build();
        let complete = self
            .inner
            .commit_builder()
            .consume_proposal_store(true)
            .load_psks(identity.provider.storage())
            .map_err(|e| SweepAttemptError::Fatal(Error::Group(format!("sweep load_psks: {e:?}"))))?
            .build(
                identity.provider.rand(),
                identity.provider.crypto(),
                &identity.signature_keys,
                |qp| selection.contains(qp.proposal_reference_ref()),
            )
            .map_err(|e| SweepAttemptError::Refused(Error::Group(format!("sweep build: {e:?}"))))?;
        let bundle = complete.stage_commit(&identity.provider).map_err(|e| {
            SweepAttemptError::Fatal(Error::Group(format!("sweep stage_commit: {e:?}")))
        })?;

        // Read what the commit ACTUALLY carried off the staged commit, before
        // merging clears it. Deriving this rather than predicting it means the
        // report and the commit cannot disagree — and it is the ONLY thing a
        // caller learns about an exclusion, deliberately: a proposal dropped by
        // the search is simply absent from `swept`, exactly as one dropped by
        // the enumeration is. The two are not distinguished because there is
        // nothing to distinguish them WITH — `CreateCommitError` names no
        // proposal — and inventing a reason would be worse than reporting none.
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
        Ok(ProposalCommit {
            commit,
            welcome,
            swept,
        })
    }

    /// `true` when a Commit carrying exactly `selection` can be built.
    ///
    /// A probe, and it is free of consequences: `CommitBuilder::build` runs
    /// every `validate_*` before it takes `public_group.empty_diff()`, holds the
    /// group by shared reference throughout, and writes nothing — `stage_commit`
    /// is the writing half, and this never reaches it. So a failed probe leaves
    /// the group, its epoch and its proposal store exactly as they were, and a
    /// successful one throws away a commit nobody will ever see.
    fn sweep_selection_builds(&mut self, identity: &Identity, selection: &[ProposalRef]) -> bool {
        count_sweep_build();
        let Ok(loaded) = self
            .inner
            .commit_builder()
            .consume_proposal_store(true)
            .load_psks(identity.provider.storage())
        else {
            return false;
        };
        loaded
            .build(
                identity.provider.rand(),
                identity.provider.crypto(),
                &identity.signature_keys,
                |qp| selection.contains(qp.proposal_reference_ref()),
            )
            .is_ok()
    }

    /// Find, by blind exclusion, a subset of `candidates` this device can
    /// actually commit.
    ///
    /// See [`Group::stage_selection`] for why the search exists and why it is
    /// additive rather than leave-one-out. Two flat passes, so it terminates on
    /// any input and costs at most `2 * candidates.len()` builds — bounded in
    /// turn by the pending-set cap
    /// ([`MAX_PENDING_PROPOSALS`] + [`MAX_RESERVED_OWNER_RESPONSES`]), which
    /// both writers into the store enforce.
    ///
    /// A candidate must clear two gates, not one. Building is the obvious gate.
    /// The other is [`crate::group_proposals::owner_survives`], re-asked over
    /// each trial selection, because it is a property of the commit as a WHOLE:
    /// the enumeration blessed a role change against the full candidate set, and
    /// dropping an `Add` from that set can invalidate it. Without this the
    /// search could assemble a perfectly buildable commit naming an owner no
    /// leaf holds — which every peer refuses ([`authorise_role_commit`]),
    /// trading a local wedge for a fork.
    fn narrow_sweep_by_exclusion(
        &mut self,
        identity: &Identity,
        queued: &[QueuedProposal],
        members: &[MemberRecord],
        candidates: &[ProposalRef],
    ) -> Vec<ProposalRef> {
        let mut accepted: Vec<ProposalRef> = Vec::new();
        let mut rejected: Vec<ProposalRef> = Vec::new();
        for candidate in candidates {
            if self.selection_admits(identity, queued, members, &accepted, candidate) {
                accepted.push(candidate.clone());
            } else {
                rejected.push(candidate.clone());
            }
        }
        // Second pass. A candidate refused above may have been refused only
        // because what it depends on had not been offered yet — a re-`Add`
        // ahead of the `Remove` that frees its signature key, an ownership
        // transfer ahead of the `Add` that puts its owner in the group. One
        // re-offer against the finished selection recovers those; it can never
        // lose anything, since each acceptance is verified in full.
        for candidate in rejected {
            if self.selection_admits(identity, queued, members, &accepted, &candidate) {
                accepted.push(candidate);
            }
        }
        accepted
    }

    /// `true` when `accepted` plus `candidate` is a selection this device may
    /// commit — it keeps the group owned, and it builds.
    ///
    /// The cheap gate runs first: `owner_survives` reads proposals already in
    /// hand, while a build derives a whole epoch's key schedule.
    fn selection_admits(
        &mut self,
        identity: &Identity,
        queued: &[QueuedProposal],
        members: &[MemberRecord],
        accepted: &[ProposalRef],
        candidate: &ProposalRef,
    ) -> bool {
        let mut trial = accepted.to_vec();
        trial.push(candidate.clone());
        if !self.selection_is_admissible(queued, &trial, members) {
            return false;
        }
        self.sweep_selection_builds(identity, &trial)
    }

    /// `true` when committing exactly `selection` passes every commit-wide rule
    /// a peer applies: it keeps an owner in the group
    /// ([`crate::group_proposals::owner_survives`]), leaves the conversation's
    /// kind as it was, and — in a one-to-one — keeps its two people
    /// ([`crate::group_proposals::one_to_one_holds`], ADR-0060). The
    /// per-proposal half is the enumeration's; these are the rules only the
    /// whole selection can answer.
    fn selection_is_admissible(
        &self,
        queued: &[QueuedProposal],
        selection: &[ProposalRef],
        members: &[MemberRecord],
    ) -> bool {
        let owner = self.group_roles().map(|t| *t.owner()).unwrap_or_default();
        let effects = commit_effects(
            queued
                .iter()
                .filter(|qp| selection.contains(qp.proposal_reference_ref())),
        );
        owner_survives(&effects, members, &owner).is_ok()
            && group_kind_unchanged(&effects, &self.kind_state()).is_ok()
            && one_to_one_holds(&effects, members, self.group_kind()).is_ok()
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

    /// Export a secret from the epoch a staged sweep **moves to** (N+1), while
    /// this device still sits at N. `Ok(None)` when nothing is staged.
    ///
    /// Read-only: it derives from the staged commit's own epoch secrets and
    /// changes nothing in the group or the provider. What it exists for is
    /// [ADR-0053]: a committer must register the Delivery Service access key
    /// of epoch N+1 **before** publishing the commit that reaches it (proven
    /// under the key of N, which it still holds), so no peer that applies the
    /// commit is ever left holding an epoch the server does not know. On an
    /// eager-merge path the same key comes from [`Group::export_secret`] after
    /// the create call; only the staged path needed this.
    ///
    /// The bytes equal what [`Group::export_secret`] returns for the same
    /// arguments after [`Group::merge_staged_sweep`], which the property test
    /// pins; if the sweep is abandoned they name an epoch that never existed,
    /// and ADR-0053's replace rule lets the real N+1 overwrite the registration.
    ///
    /// [ADR-0053]: ../../../docs/adr/0053-ds-journal-authorisation.md
    ///
    /// # Errors
    ///
    /// [`Error::Exporter`] on an OpenMLS exporter failure (a staged commit that
    /// removes this device, or a zero / oversized length).
    pub fn staged_export_secret(
        &self,
        identity: &Identity,
        label: &str,
        context: &[u8],
        length: usize,
    ) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let Some(staged) = self.inner.pending_commit() else {
            return Ok(None);
        };
        let secret = staged
            .export_secret(identity.provider.crypto(), label, context, length)
            .map_err(|e| Error::Exporter(format!("{e:?}")))?;
        Ok(Some(Zeroizing::new(secret)))
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

    /// Process one row read from this group's **knock** queue ([ADR-0053]).
    ///
    /// The knock is the one unauthenticated write to a claimed group - anyone
    /// who pays the proof of work can put bytes there, a member included - so
    /// a knock row may be anything. The only thing that belongs there is a
    /// non-member's external-join proposal for this group: a `PublicMessage`
    /// whose content is a proposal and whose sender is `NewMemberProposal`.
    /// Everything else is refused **before** OpenMLS processes it. Without the
    /// gate a member could post a genuine commit or application message to
    /// the knocks instead of the journal; the admins, who read knocks, would
    /// apply it and every other member would not - a fork chosen by whoever
    /// wrote the row. Written once here so three clients cannot filter three
    /// ways.
    ///
    /// A row that passes goes through [`Group::process_incoming_verifying_roles`]
    /// unchanged, so the ADR-0027 quarantine and its role gate apply as for a
    /// journal row.
    ///
    /// [ADR-0053]: ../../../docs/adr/0053-ds-journal-authorisation.md
    ///
    /// # Errors
    ///
    /// [`Error::Message`] for a row that is not an external-join proposal for
    /// this group (the caller drops it), plus whatever processing returns.
    pub fn process_knock(&mut self, identity: &Identity, bytes: &[u8]) -> Result<Incoming> {
        if !is_external_join_proposal_for(bytes, self.inner.group_id().as_slice()) {
            return Err(Error::Message(
                "knock row is not an external-join proposal for this group".to_string(),
            ));
        }
        self.process_incoming_verifying_roles(identity, bytes)
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
                sender_account: committer_leaf.and_then(|l| self.account_key_at(l)),
            }),
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                // Read the role table from the PRE-MERGE GroupContext. `None` =>
                // an open (legacy) group, no enforcement.
                if let Some(table) = self.group_roles() {
                    let members = self.members();
                    authorise_role_commit(&table, &members, &self.kind_state(), &staged)?;
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
                self.retain_external_join(identity, &qp)
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
        let proposer = sender_leaf.and_then(|leaf| proposer_at(&members, leaf));
        // ADR-0060: the early one-to-one answer, and no rewrite that changes
        // the kind, in either kind of conversation. Neither is ever stored, so
        // neither can sit in the pending set or reach a sweep.
        if one_to_one_allows(self.group_kind(), &members, proposer, &kind).is_err()
            || !proposal_keeps_group_kind(qp.proposal(), &self.kind_state())
        {
            return refused(kind);
        }
        if authorise(&table, &members, proposer, &kind).is_err() {
            return refused(kind);
        }
        if self
            .refuse_if_device_limit_reached(&members, &kind)
            .is_err()
        {
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
        //
        // The capacity is the owner-reserve-aware one, and it MUST be: this is
        // the gate the owner's own peers run, so refusing the owner's response
        // here would leave the owner holding a proposal nobody else does and
        // sweeping a commit nobody can resolve. Authorship is the
        // MLS-authenticated sender leaf resolved against the pre-merge roster
        // just above, so the reserve is not claimable by anyone else.
        let capacity = pending_capacity(is_owner_response(&table, proposer, &kind));
        if self.inner.pending_proposals().count() >= capacity {
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
                sender_account: sender_leaf.and_then(|l| self.account_key_at(l)),
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
                self.retain_external_join(identity, &qp)
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
                sender_account: sender_leaf.and_then(|l| self.account_key_at(l)),
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
                self.retain_external_join(identity, &qp)
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

    /// Whether this device is still a member of the group.
    ///
    /// `false` means a Commit that removed this device's own leaf has been
    /// applied. RFC 9420 gives no way back: OpenMLS calls the resulting state
    /// *"a terminal state that the group can not exit from"*, sets it inside
    /// `merge_staged_commit` on `StagedCommit::self_removed()`, and **persists
    /// it before merging** — so it survives a snapshot / restore round trip and
    /// this accessor answers the same at launch as it did in the poll batch
    /// that carried the removal. Re-joining is a new `Group` (a Welcome, or an
    /// external commit), never this one.
    ///
    /// Nothing in the receive path signals the transition:
    /// [`Self::process_incoming_verifying_roles`] returns the same content-less
    /// [`Incoming::Commit`] for a commit that evicts this device as for any
    /// other. **This accessor is the only signal.**
    ///
    /// # What still works, and what stops
    ///
    /// The asymmetry is the whole point, and it is why an evicted group should
    /// stay readable in the UI while every action on it is withdrawn:
    ///
    /// * **Reads keep working, frozen forever.** [`Self::members`],
    ///   [`Self::epoch`], [`Self::member_count`], [`Self::own_leaf_index`],
    ///   [`Self::group_id`] and [`Self::group_roles`] all still answer, at the
    ///   values the removing Commit produced. They will never change again.
    /// * **Nothing further can be decrypted** — OpenMLS refuses
    ///   `unprotect_message` on `!is_active()` before it inspects the message
    ///   at all, so this is *not* epoch-scoped: application rows still sitting
    ///   unfetched on the Delivery Service are unreadable even when they were
    ///   sealed at an epoch whose keys this device still holds.
    /// * **Sending and every exporter read fail.**
    ///   [`Self::create_application_message`] and [`Self::export_secret`] both
    ///   raise `UseAfterEviction`, which reaches beyond the composer: the
    ///   per-call SFrame seed, the push routing-secret broadcast and the
    ///   committer beacon are all one or the other.
    /// * **The reported epoch moves while the secrets do not.** The merge takes
    ///   OpenMLS's public-state branch, so [`Self::epoch`] reports N+1 over
    ///   epoch-N secrets that were never replaced. Consult this accessor rather
    ///   than reasoning from the epoch.
    ///
    /// # What a caller is expected to do with it
    ///
    /// Gate on it wherever the group is acted upon — disable the composer and
    /// the membership affordances, stop polling, stop emitting beacons — and
    /// keep rendering the history, which is still readable and still true. A
    /// caller that instead infers membership from "my signature key is absent
    /// from [`Self::members`]" gets the same answer today, but it is a
    /// different claim: this reads the state the refusals themselves consult,
    /// and it does not rest on a leaf's signature key being the account's
    /// (ADR-0022 names the per-device-leaf split as an open follow-up, which
    /// would break that inference silently).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.inner.is_active()
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
            .map(|m| {
                let account_key = crate::device_binding::account_key_of_credential(
                    m.credential.serialized_content(),
                    &m.signature_key,
                )
                .map(|k| k.to_vec())
                .unwrap_or_default();
                MemberRecord {
                    leaf_index: m.index.u32(),
                    signature_public_key: m.signature_key,
                    account_key,
                }
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
        Ok(loaded.map(|inner| Self {
            inner,
            pending_external_joins: Self::load_pending_external_joins(identity, group_id),
        }))
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

/// The `required_capabilities` a conversation carries from [`Group::create`]:
/// the ADR-0022 role table (`0xFF02`) and the ADR-0060 conversation kind
/// (`0xFF03`). Naming the kind here means a device whose KeyPackage does not
/// advertise it can never be added to a conversation, so every member's device
/// understands — and enforces — the kind.
fn conversation_required_capabilities() -> Extension {
    Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
        &[
            ExtensionType::Unknown(ROLE_EXTENSION_TYPE),
            ExtensionType::Unknown(GROUP_KIND_EXTENSION_TYPE),
        ],
        &[],
        &[],
    ))
}

/// Refuse a Welcome into a conversation that does not say, readably, what kind
/// it is (ADR-0060 section 3): a GroupContext carrying the ADR-0022 role table
/// but no `0xFF03`, or an `0xFF03` body that does not decode. A group with no
/// role table is not a conversation (`PublisherGroup`, `DeviceGroup`) and is
/// not asked.
fn refuse_unclassifiable_conversation(extensions: &Extensions<GroupContext>) -> Result<()> {
    if extensions.unknown(ROLE_EXTENSION_TYPE).is_none() {
        return Ok(());
    }
    let Some(body) = extensions.unknown(GROUP_KIND_EXTENSION_TYPE) else {
        return Err(Error::Group(
            "welcome refused: the conversation does not carry its kind".into(),
        ));
    };
    GroupKind::decode(&body.0).map(|_| ()).map_err(|e| {
        Error::Group(format!(
            "welcome refused: unreadable conversation kind: {e}"
        ))
    })
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
    let proposer = leaf.and_then(|leaf| proposer_at(members, leaf));
    authorise(table, members, proposer, &classify(qp.proposal())).is_ok()
}

/// Why one attempt to stage a sweep did not produce a commit.
///
/// The distinction is the whole reason the exclusion search is safe to run:
/// [`SweepAttemptError::Refused`] means `CommitBuilder::build` rejected THIS
/// selection and wrote nothing, so a different selection may be tried on a
/// group that is byte-for-byte where it was. Anything else is a failure a
/// different selection cannot fix, and is surfaced immediately.
enum SweepAttemptError {
    /// `CommitBuilder::build` refused the selection. Nothing was written.
    Refused(Error),
    /// A failure outside the build — loading PSKs, or staging the commit.
    /// Do not retry.
    Fatal(Error),
}

impl SweepAttemptError {
    /// Discard the distinction and surface the underlying error.
    fn into_error(self) -> Error {
        match self {
            Self::Refused(e) | Self::Fatal(e) => e,
        }
    }
}

// Counts every `CommitBuilder::build` the sweep path performs, in test builds
// only. The cost of the exclusion search is a claim this crate makes in prose,
// and a claim about a count is worth exactly as much as the counter that checks
// it.
#[cfg(test)]
thread_local! {
    static SWEEP_BUILD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Record one `CommitBuilder::build` performed by the sweep path.
///
/// Compiles to nothing outside tests.
#[inline]
fn count_sweep_build() {
    #[cfg(test)]
    SWEEP_BUILD_COUNT.with(|count| count.set(count.get() + 1));
}

/// Zero the sweep build counter.
#[cfg(test)]
fn reset_sweep_builds() {
    SWEEP_BUILD_COUNT.with(|count| count.set(0));
}

/// Builds performed by the sweep path since [`reset_sweep_builds`].
#[cfg(test)]
fn sweep_builds() -> usize {
    SWEEP_BUILD_COUNT.with(std::cell::Cell::get)
}

/// Everything [`sweep_keeps`] needs that is the same for every proposal in one
/// sweep. Held apart from [`SweepState`] so the "shared context, fresh state"
/// discipline is visible at the call sites: the emptiness prediction and the
/// real filter share this and must not share that.
struct SweepContext<'a> {
    /// The pre-merge role table every proposal is judged against.
    table: &'a RoleTable,
    /// The pre-merge roster.
    members: &'a [MemberRecord],
    /// The pre-merge group-kind facts (ADR-0060): a role change that would
    /// install anything else is never swept.
    kind_before: &'a KindState,
    /// Whether the authorised set's role change would leave the group with an
    /// owner inside it (`owner_survives`, computed once over the whole set).
    role_change_survives: bool,
    /// Leaves the authorised set removes. Removes and leaves are never excluded
    /// by this filter, so this is exactly the set the emitted commit removes.
    removed_leaves: &'a [u32],
    /// What this group demands of a leaf before it may join
    /// (`Group::admission_requirements`), read before the commit builder
    /// borrowed the group.
    admission: &'a GroupAdmission,
    /// The sweeping device, for the crypto provider
    /// [`added_leaf_can_join`] re-validates a KeyPackage with. Nothing about
    /// this device enters the decision — a filter whose answer depended on
    /// *who* swept would make the sweep's contents unpredictable to every peer.
    identity: &'a Identity,
}

/// What a group demands of a prospective leaf, snapshotted at one epoch.
///
/// Every field is a group property, never a proposal property: two committers
/// at the same epoch necessarily build the identical value, so
/// [`added_leaf_can_join`] is a function of the group and the KeyPackage alone
/// and every honest committer reaches the same verdict on the same proposal.
struct GroupAdmission {
    /// The group's ciphersuite.
    ciphersuite: Ciphersuite,
    /// The group's MLS protocol version.
    protocol_version: ProtocolVersion,
    /// Every extension type present in the authenticated `GroupContext`.
    group_extension_types: Vec<ExtensionType>,
    /// Extension types the group's `required_capabilities` demands.
    required_extension_types: Vec<ExtensionType>,
    /// Proposal types the group's `required_capabilities` demands.
    required_proposal_types: Vec<ProposalType>,
    /// Credential types the group's `required_capabilities` demands.
    required_credential_types: Vec<CredentialType>,
    /// Credential types in use by members the commit is **not** removing,
    /// de-duplicated. A joining leaf must advertise support for all of them.
    staying_credential_types: Vec<CredentialType>,
    /// HPKE encryption keys of members the commit is **not** removing. RFC 9420
    /// §12.2 requires these to stay unique across the group.
    staying_encryption_keys: Vec<EncryptionKey>,
}

/// What one pass of [`sweep_keeps`] has committed to so far.
///
/// Each pass over the pending set needs its **own** value: the emptiness
/// prediction and the real filter must not share one, or the prediction would
/// spend the budget the filter needs.
#[derive(Default)]
struct SweepState {
    /// A role change has already been taken by this pass (at most one is
    /// admissible per commit).
    role_change_taken: bool,
    /// Signature keys of the `Add` proposals this pass has taken (each key is
    /// admissible at most once per commit).
    kept_added_keys: Vec<Vec<u8>>,
    /// The accounts of the `Add` proposals this pass has taken (ADR-0050 —
    /// the per-account leaf cap counts them alongside the staying roster).
    kept_added_accounts: Vec<Vec<u8>>,
    /// HPKE **encryption** keys of the `Add` proposals this pass has taken.
    kept_encryption_keys: Vec<EncryptionKey>,
    /// HPKE **init** keys of the `Add` proposals this pass has taken, held as
    /// [`EncryptionKey`] so the two sets are directly comparable.
    kept_init_keys: Vec<EncryptionKey>,
}

/// `true` when the ADR-0045 sweep should carry `qp`.
///
/// Every clause below is an **exclusion, not a refusal**: a proposal dropped
/// here is simply not committed, its author re-proposes, and the rest of the
/// sweep still lands. Refusing the whole sweep instead would let one bad
/// proposal block every unrelated membership change in the group — and, because
/// the proposal store only empties when a commit is *merged* and every
/// direct-commit path is closed while proposals are pending
/// ([`Group::refuse_if_proposals_pending`]), a sweep that cannot be built is not
/// a lost round but a **permanent membership wedge**: no commit can be produced,
/// so the store can never be emptied, so no commit can ever be produced.
///
/// 1. **Authorised for its author**, judged against the table as it stands
///    (`proposal_is_authorised`).
/// 2. **At most one role change**, and only one that survives
///    [`owner_survives`]. RFC 9420 admits one `GroupContextExtensions` proposal
///    per commit and OpenMLS enforces it as a hard build failure
///    (`TooManyGCEProposals`), so a set holding two cannot be swept at all.
///    [`Group::propose_group_roles`] keeps a well-behaved device from building
///    that set; this keeps a set that reached it anyway — a peer whose proposal
///    was published before the crash that lost its record of it, say — from
///    wedging the whole group.
/// 3. **At most one `Add` per signature key, and none that collides with a
///    member the commit is not removing.** RFC 9420 §12.2 requires the group's
///    signature keys to be unique, and OpenMLS enforces it in exactly the same
///    hard-build-failure shape as the role-change rule
///    (`validate_key_uniqueness` → `ProposalValidationError::DuplicateSignatureKey`,
///    reached from `CommitBuilder::build`). Without this clause the wedge is
///    reachable from **one ordinary proposal**: an admin proposing to add
///    somebody who is already a member — a stale roster, a double tap — or two
///    admins independently proposing the same person, which is precisely the
///    concurrent composition ADR-0045 exists to make safe. It is also the
///    cheapest form of the flood the owner reserve answers, since an attacker
///    can mint unlimited distinct KeyPackages for one signature key.
///
///    The collision is judged against the roster the commit **produces**, so a
///    key belonging to a member this same commit removes does not collide —
///    matching `validate_key_uniqueness`, which seeds its sets from
///    `full_leaf_members()` filtered by the commit's remove proposals, and so
///    keeping a legitimate remove-and-re-add working.
///
///    Excluding a duplicate cannot change [`owner_survives`]' answer, which is
///    computed before this runs: a key dropped as a duplicate is still added by
///    the copy that was kept, and a key dropped for colliding with a member is
///    by construction a member the commit keeps.
/// 4. **The added leaf must be able to live in THIS group**
///    ([`added_leaf_can_join`]). Clause 3 covers the one thing the added leaf
///    shares with the group — its signature key — and leaves everything the
///    group *demands* of a leaf uncovered: `validate_add_proposals` also refuses
///    a build outright when the added leaf does not advertise every extension
///    type the GroupContext carries, does not satisfy `required_capabilities`,
///    does not list the group's own ciphersuite or version, is not internally
///    self-consistent, or carries an expired lifetime.
///
///    An incoming `Add` clears only `AddProposalIn::validate` on the way in
///    (KeyPackage signature, protocol version, ciphersuite), which asks nothing
///    about the group, so a KeyPackage that is perfectly well-formed and simply
///    unsuited to this group is authorised, retained, and then wedges every
///    sweep. Two of those conditions need no adversary whatsoever: a peer whose
///    KeyPackage was minted with a different capability set is a conformant
///    RFC 9420 client, and a **lifetime expires by the passage of time alone**
///    — ADR-0045 gives a request no deadline, so a proposal may outlive the
///    KeyPackage it names.
/// 5. **Encryption and init keys unique across the roster the commit
///    produces**, the remaining arms of `validate_key_uniqueness` that clause 3
///    does not reach (`DuplicateEncryptionKey`, `DuplicateInitKey`,
///    `InitEncryptionKeyCollision`). Same hard-build-failure shape, same
///    permanent wedge; unlike clause 3 these are unreachable from a KeyPackage
///    this crate mints, because every one carries freshly generated keys.
///    `Update` proposals contribute encryption keys to the same sets upstream,
///    and contribute none here for the reason clause 1 gives: every `Update` is
///    refused by [`crate::group_proposals::authorise`], so none ever reaches a
///    sweep.
///
/// **What a kept-but-uncommittable proposal costs**, which is why clauses 2–5
/// exist at all: the failure is not the proposal's, it is the **group's**. One
/// unbuildable proposal means no commit can be produced; no commit means the
/// proposal store is never emptied; a non-empty store means every direct commit
/// is refused too — so every add, removal, role change and external-join
/// approval in that group stops, for everyone, permanently, with no path back
/// short of another admin's device that somehow lacks the offending entry. And
/// it is **invisible**: chat keeps working throughout
/// ([`Group::create_application_message`] parks the pending set around the
/// encrypt), so the group looks healthy while its membership is bricked.
///
/// Which of a colliding pair is kept is the store's own order and is arbitrary;
/// the one dropped is dropped from every member's store when the commit merges,
/// so its author must re-propose. The caller learns what survived from
/// [`ProposalCommit::swept`], which is read back off the staged commit.
fn sweep_keeps(ctx: &SweepContext<'_>, state: &mut SweepState, qp: &QueuedProposal) -> bool {
    if !proposal_is_authorised(ctx.table, ctx.members, qp) {
        return false;
    }
    match classify(qp.proposal()) {
        ProposalKind::Add {
            signature_key,
            account_key,
        } => {
            if state.kept_added_keys.contains(&signature_key) {
                return false;
            }
            // ADR-0050: an unresolvable binding never joins, and an account
            // never exceeds its leaf cap — counted over the staying roster plus
            // the Adds this pass has already taken for the same account.
            let Some(account) = account_key else {
                return false;
            };
            let staying = ctx
                .members
                .iter()
                .filter(|m| m.account_key == account && !ctx.removed_leaves.contains(&m.leaf_index))
                .count();
            let taken = state
                .kept_added_accounts
                .iter()
                .filter(|a| **a == account)
                .count();
            if staying + taken >= MAX_LEAVES_PER_ACCOUNT {
                return false;
            }
            state.kept_added_accounts.push(account);
            let collides_with_a_staying_member = ctx.members.iter().any(|m| {
                m.signature_public_key == signature_key
                    && !ctx.removed_leaves.contains(&m.leaf_index)
            });
            if collides_with_a_staying_member {
                return false;
            }
            // `classify` reported an `Add`, so the proposal is one; a variant
            // mismatch here would be a `classify` bug, and dropping is the safe
            // reading of "we cannot tell what this is".
            let Proposal::Add(add) = qp.proposal() else {
                return false;
            };
            let key_package = add.key_package();
            if !added_leaf_can_join(ctx, key_package) {
                return false;
            }
            // Both keys are normalised to `EncryptionKey` so the "no key may
            // serve as both an init key and an encryption key" arm compares
            // like with like; the type carries no byte accessor, but it is
            // `Clone + Eq` and converts from raw bytes, which is all this needs.
            let encryption_key = key_package.leaf_node().encryption_key().clone();
            let init_key = EncryptionKey::from(key_package.hpke_init_key().as_slice().to_vec());
            // Clause 5. The three sets OpenMLS keeps: encryption keys unique
            // across the produced roster and the kept adds; init keys unique
            // across the kept adds; and no key serving as both. Checked in both
            // directions because the pass is incremental — a later add's
            // encryption key must not equal an earlier add's init key either.
            let encryption_key_is_taken = ctx
                .admission
                .staying_encryption_keys
                .contains(&encryption_key)
                || state.kept_encryption_keys.contains(&encryption_key)
                || state.kept_init_keys.contains(&encryption_key);
            let init_key_is_taken = ctx.admission.staying_encryption_keys.contains(&init_key)
                || state.kept_encryption_keys.contains(&init_key)
                || state.kept_init_keys.contains(&init_key)
                || init_key == encryption_key;
            if encryption_key_is_taken || init_key_is_taken {
                return false;
            }
            state.kept_added_keys.push(signature_key);
            state.kept_encryption_keys.push(encryption_key);
            state.kept_init_keys.push(init_key);
            true
        },
        ProposalKind::RoleChange => {
            // ADR-0060: no rewrite that changes the kind is ever swept, and in
            // a one-to-one no rewrite is swept at all.
            if !ctx.role_change_survives
                || state.role_change_taken
                || !proposal_keeps_group_kind(qp.proposal(), ctx.kind_before)
                || ctx.kind_before.kind() == Some(GroupKind::OneToOne)
            {
                return false;
            }
            state.role_change_taken = true;
            true
        },
        _ => true,
    }
}

/// `true` when `key_package`'s leaf can be committed into this group — the
/// [`sweep_keeps`] clause-4 predicate.
///
/// **This is not a permission check.** [`crate::group_proposals::authorise`] has
/// already run and decides *who may ask*; this decides only *what is
/// committable*, which is a property of the KeyPackage and the group and of
/// nothing else. It can therefore never widen or narrow who may propose an add.
///
/// It mirrors, at sweep time, what `CommitBuilder::build` would refuse
/// outright for an `Add` (openmls-0.8.1 `public_group/validation.rs`):
///
/// * `validate_add_proposals` / ValSem105 — the KeyPackage's ciphersuite and
///   protocol version must match the group's;
/// * valn0502 — the added leaf must advertise every extension type present in
///   the `GroupContext`;
/// * `validate_leaf_node` → `validate_leaf_node_capabilities`:
///   valn1601 / valn0113 (`LeafNode::validate_locally`: every extension the
///   leaf carries must be legal in a leaf node and covered by its own
///   capabilities, which must also cover its own credential type), valn0107
///   (the leaf must list the group's ciphersuite and version), valn0103 (the
///   group's `required_capabilities`), valn0104 (credential types must be
///   mutually supported);
/// * valn0106 — the KeyPackage's **lifetime** must still be current.
///
/// # Two halves, and why they are checked differently
///
/// The **KeyPackage-intrinsic** half is delegated straight back to OpenMLS:
/// re-serialising and re-running `KeyPackageIn::validate` at the group's
/// protocol version is the library's own rule, so it cannot drift from it, and
/// it is the only way to reach the lifetime at all (`LeafNode::life_time` is
/// `pub(crate)`). It re-verifies two signatures that were already verified on
/// the way in; at [`MAX_PENDING_PROPOSALS`] that is a bounded cost paid only
/// when a membership change is actually being committed, and it buys the one
/// condition that changes **while the proposal sits in the store**.
///
/// The **group-relative** half cannot be delegated — the private
/// `PublicGroup::validate_*` methods are the only implementation and they need
/// a group OpenMLS will not lend us — so it is restated here against
/// [`GroupAdmission`]. Each restatement falls to *reject* on anything it does
/// not recognise (see [`extension_type_is_default`] and its siblings), because
/// wrongly keeping a proposal wedges the group permanently while wrongly
/// dropping one costs its author a re-proposal.
///
/// # What this deliberately does not check
///
/// * **Every existing member's capabilities must contain the added leaf's
///   credential type** (the other half of valn0104). OpenMLS exposes a member's
///   credential but not its capabilities, so this is not decidable from public
///   API. It is also not a property of the added leaf: it fails only when the
///   group already contains a member that cannot accept the joiner's credential
///   type, which no Lakko client can produce — every identity mints a `Basic`
///   credential and advertises the default credential set.
/// * **A commit path's own encryption keys** (ValSem206/207). They are freshly
///   generated by the committer inside `build` and are not visible to a
///   proposal filter; a collision would be a random-number-generator event.
///
/// # The one input that is not group state
///
/// The lifetime makes this predicate a function of the clock, with two
/// consequences worth naming rather than discovering:
///
/// * [`Group::stage_pending_proposals`] consults it **once**, recording the
///   verdict as a set of [`ProposalRef`]s that every later build selects by
///   membership, so two evaluations of this predicate can no longer disagree
///   within one sweep. What remains is a microseconds-wide window between the
///   last exclusion probe and the build that is finally staged: a KeyPackage
///   expiring in it makes that build fail, which surfaces as an error and is
///   repaired by the next sweep — where this predicate, re-run, excludes it by
///   name.
/// * A peer processing a commit re-runs the same lifetime check
///   (`staged_commit.rs` calls `validate_add_proposals` too), so an `Add`
///   committed moments before expiry can still be refused by a peer that
///   processes it moments after. That is RFC 9420's own behaviour for a
///   near-expiry KeyPackage and predates this filter; excluding an
///   already-expired one strictly narrows the window rather than opening it.
fn added_leaf_can_join(ctx: &SweepContext<'_>, key_package: &KeyPackage) -> bool {
    let admission = ctx.admission;
    // ValSem105, first half. The second half (protocol version) is not readable
    // from a `KeyPackage` — `KeyPackage::protocol_version` is `pub(crate)` — and
    // is covered by the re-validation below, which takes the version as its
    // argument.
    if key_package.ciphersuite() != admission.ciphersuite {
        return false;
    }

    // The KeyPackage-intrinsic half, handed back to OpenMLS verbatim: leaf and
    // KeyPackage signatures, the protocol version, init key != encryption key,
    // the KeyPackage's own extensions against the leaf's capabilities, and
    // valn0106 — the lifetime, evaluated NOW rather than when the proposal
    // arrived. Any failure is a drop.
    let revalidated = crate::key_package::serialize(key_package)
        .and_then(|bytes| crate::key_package::deserialize(&bytes))
        .ok()
        .and_then(|kp_in| {
            kp_in
                .validate(ctx.identity.provider.crypto(), admission.protocol_version)
                .ok()
        });
    if revalidated.is_none() {
        return false;
    }

    let leaf = key_package.leaf_node();
    let capabilities = leaf.capabilities();

    // valn1601 — an extension the leaf carries must be legal in a leaf node.
    if !leaf
        .extensions()
        .iter()
        .all(|e| extension_type_is_valid_in_leaf_node(e.extension_type()))
    {
        return false;
    }
    // valn0113, first half — and everything the leaf carries must be covered by
    // its own capabilities.
    if !leaf
        .extensions()
        .iter()
        .all(|e| capabilities_contain_extension(capabilities, e.extension_type()))
    {
        return false;
    }
    // valn0113, second half — including its own credential type.
    if !capabilities
        .credentials()
        .contains(&leaf.credential().credential_type())
    {
        return false;
    }
    // valn0107 — the leaf must be able to speak this group's ciphersuite and
    // version.
    if !capabilities
        .ciphersuites()
        .contains(&VerifiableCiphersuite::from(admission.ciphersuite))
        || !capabilities
            .versions()
            .contains(&admission.protocol_version)
    {
        return false;
    }
    // valn0502 — and to carry every extension the GroupContext holds.
    if !admission
        .group_extension_types
        .iter()
        .all(|t| capabilities_contain_extension(capabilities, *t))
    {
        return false;
    }
    // valn0103 — the group's `required_capabilities`, in all three dimensions.
    if !admission
        .required_extension_types
        .iter()
        .all(|t| capabilities_contain_extension(capabilities, *t))
    {
        return false;
    }
    if !admission
        .required_proposal_types
        .iter()
        .all(|t| proposal_type_is_default(*t) || capabilities.proposals().contains(t))
    {
        return false;
    }
    if !admission
        .required_credential_types
        .iter()
        .all(|t| capabilities.credentials().contains(t))
    {
        return false;
    }
    // valn0104, the half that is decidable — the joiner must accept every
    // credential type already in use by a member the commit keeps.
    admission
        .staying_credential_types
        .iter()
        .all(|t| capabilities.credentials().contains(t))
}

/// `capabilities` cover `extension_type`, matching OpenMLS's private
/// `Capabilities::contains_extension`: a default extension type needs no
/// advertisement, anything else must be listed.
fn capabilities_contain_extension(
    capabilities: &openmls::prelude::Capabilities,
    extension_type: ExtensionType,
) -> bool {
    extension_type_is_default(extension_type) || capabilities.extensions().contains(&extension_type)
}

/// Restatement of OpenMLS's private `ExtensionType::is_default`.
///
/// Written as a positive `matches!` rather than an exhaustive `match` so that an
/// extension type this build does not know about answers **`false`** — "not a
/// default, so the leaf must advertise it explicitly", which drops an `Add` we
/// cannot judge instead of keeping one that would wedge the sweep.
fn extension_type_is_default(extension_type: ExtensionType) -> bool {
    matches!(
        extension_type,
        ExtensionType::ApplicationId
            | ExtensionType::RatchetTree
            | ExtensionType::RequiredCapabilities
            | ExtensionType::ExternalPub
            | ExtensionType::ExternalSenders
    )
}

/// Restatement of OpenMLS's private `ExtensionType::is_valid_in_leaf_node`.
///
/// Same fail-toward-dropping shape as [`extension_type_is_default`]: an
/// unrecognised extension type reads as **not** legal in a leaf node.
fn extension_type_is_valid_in_leaf_node(extension_type: ExtensionType) -> bool {
    matches!(
        extension_type,
        ExtensionType::Unknown(_) | ExtensionType::ApplicationId
    )
}

/// Restatement of OpenMLS's private `ProposalType::is_default`.
///
/// Same fail-toward-dropping shape: an unrecognised proposal type reads as
/// non-default, so a leaf must advertise it to satisfy a group that requires it.
fn proposal_type_is_default(proposal_type: ProposalType) -> bool {
    matches!(
        proposal_type,
        ProposalType::Add
            | ProposalType::Update
            | ProposalType::Remove
            | ProposalType::PreSharedKey
            | ProposalType::Reinit
            | ProposalType::ExternalInit
            | ProposalType::GroupContextExtensions
    )
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
    kind_before: &KindState,
    staged: &StagedCommit,
) -> Result<()> {
    let effects = commit_effects(staged.queued_proposals());
    // The rules that are about the commit rather than about any one proposal.
    // First: whatever else this commit does, it must not leave the group with
    // an owner who is not in it. See `owner_survives` for the two ways that
    // happens and why neither is visible to a per-proposal check.
    owner_survives(&effects, members, table.owner())?;
    // ADR-0060: no commit changes the conversation's kind, and a one-to-one's
    // commit keeps its two people and rewrites nothing — judged against the
    // pre-merge GroupContext, like the role table.
    group_kind_unchanged(&effects, kind_before)?;
    one_to_one_holds(&effects, members, kind_before.kind())?;
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
        let author = author_leaf.and_then(|leaf| proposer_at(members, leaf));
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

/// The epoch the group is **in after** this journal row, read from the public
/// framing like [`epoch_of_message`] — the number a joiner must put on its
/// external-join proposal when this row is the journal head.
///
/// The two differ on a Commit: a Commit is framed at the epoch it was created
/// in, and applying it moves the group to the next one, so a journal whose
/// head is a Commit is one epoch ahead of what [`epoch_of_message`] reports.
/// A proposal crafted for the framed epoch is then refused by every member as
/// stale, and the joiner never learns why. The content type rides the same
/// cleartext framing as the epoch, so a non-member can tell the two apart.
///
/// Returns `None` exactly where [`epoch_of_message`] does.
#[must_use]
pub fn epoch_after_message(bytes: &[u8]) -> Option<u64> {
    let mut slice = bytes;
    let msg = MlsMessageIn::tls_deserialize(&mut slice).ok()?;
    let proto = msg.try_into_protocol_message().ok()?;
    let epoch = proto.epoch().as_u64();
    Some(
        if proto.content_type() == openmls::framing::ContentType::Commit {
            epoch.saturating_add(1)
        } else {
            epoch
        },
    )
}

/// Whether `bytes` are an external-join proposal for `group_id`: a
/// `PublicMessage` carrying a proposal from a `NewMemberProposal` sender,
/// framed for that group. Reads only the cleartext framing, like
/// [`epoch_of_message`]; the gate [`Group::process_knock`] uses.
#[must_use]
pub fn is_external_join_proposal_for(bytes: &[u8], group_id: &[u8]) -> bool {
    let mut slice = bytes;
    let Ok(msg) = MlsMessageIn::tls_deserialize(&mut slice) else {
        return false;
    };
    let Ok(proto) = msg.try_into_protocol_message() else {
        return false;
    };
    if proto.group_id().as_slice() != group_id
        || proto.content_type() != openmls::framing::ContentType::Proposal
    {
        return false;
    }
    match proto {
        openmls::framing::ProtocolMessage::PublicMessage(public) => {
            matches!(public.sender(), Sender::NewMemberProposal)
        },
        openmls::framing::ProtocolMessage::PrivateMessage(_) => false,
    }
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
    /// **A filter that cannot see.** Sweep with the enumeration reduced to its
    /// FIRST clause — authorisation, and nothing else — which is byte-for-byte
    /// what `sweep_keeps` was before clauses 2-5 were written.
    ///
    /// It exists so a wedge the enumeration does not catch can be reproduced
    /// without inventing a hypothetical cause: any condition clauses 2-5
    /// currently name becomes, through this entry point, a condition the filter
    /// is blind to, and therefore a stand-in for the ones nobody has named. It
    /// skips no authorisation and grants no authority — `proposal_is_authorised`
    /// is the same call [`sweep_keeps`] makes first — so it can only ever
    /// produce a commit the real sweep could also have produced.
    ///
    /// Everything downstream is the shipped path: the same
    /// [`Group::stage_selection`], the same exclusion search, the same staging.
    fn sweep_with_authorisation_only_filter(
        &mut self,
        identity: &Identity,
    ) -> Result<Option<ProposalCommit>> {
        let table = self.role_table_or_refuse("sweep")?;
        let members = self.members();
        let candidates: Vec<ProposalRef> = self
            .inner
            .pending_proposals()
            .filter(|qp| proposal_is_authorised(&table, &members, qp))
            .map(|qp| qp.proposal_reference_ref().clone())
            .collect();
        let staged = self.stage_selection(identity, candidates)?;
        if staged.is_some() {
            self.merge_staged_sweep(identity)?;
        }
        Ok(staged)
    }

    /// **Adversary.** Propose an Add with no local authorisation check.
    fn ungated_propose_add(&mut self, identity: &Identity, kp: KeyPackage) -> Result<Vec<u8>> {
        let (proposal, _r) = self
            .inner
            .propose_add_member(&identity.provider, &identity.signature_keys, &kp)
            .map_err(|e| Error::Group(format!("ungated_propose_add: {e:?}")))?;
        encode_message(&proposal)
    }

    /// **Barely an adversary at all.** Emit a stock RFC 9420 `Update` proposal.
    ///
    /// Unlike its siblings here, this helper skips no Lakko check, because there
    /// is none to skip: `MlsGroup::propose_self_update` is ordinary OpenMLS,
    /// reachable by any conformant MLS client, and `lakko-mls` simply publishes
    /// no entry point onto it. That is precisely what makes the flood it enables
    /// reachable **without** a modified client — the reason the refusal has to
    /// live in [`crate::group_proposals::authorise`] rather than in the absence
    /// of a `propose_update` method.
    fn ungated_propose_self_update(&mut self, identity: &Identity) -> Result<Vec<u8>> {
        use openmls::prelude::LeafNodeParameters;
        let (proposal, _r) = self
            .inner
            .propose_self_update(
                &identity.provider,
                &identity.signature_keys,
                LeafNodeParameters::default(),
            )
            .map_err(|e| Error::Group(format!("ungated_propose_self_update: {e:?}")))?;
        encode_message(&proposal)
    }

    /// **Adversary.** Propose a role-table rewrite with no local check that the
    /// table it installs names an owner the group actually holds.
    ///
    /// [`Group::propose_group_roles`] refuses that before touching MLS
    /// (`refuse_unenforceable_role_table`), so an honest client cannot emit one.
    /// A modified one can, and [`crate::group_proposals::authorise`] — the rule
    /// every PEER applies on receipt — asks only *who* proposed a role change,
    /// never *whom it names*. Which is exactly why
    /// [`crate::group_proposals::owner_survives`] is a commit-level rule, and
    /// why the exclusion search has to re-ask it of every selection it tries.
    fn ungated_propose_group_roles(
        &mut self,
        identity: &Identity,
        roles: &RoleTable,
    ) -> Result<Vec<u8>> {
        let exts = self.role_rewrite_extensions(roles)?;
        let (proposal, _proposal_ref) = self
            .inner
            .propose_group_context_extensions(&identity.provider, exts, &identity.signature_keys)
            .map_err(|e| Error::Group(format!("ungated_propose_group_roles: {e:?}")))?;
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

    /// **Adversary.** Store a non-member's **external-join** Add straight into
    /// the MLS proposal store the sweep consumes — the one thing
    /// [`Group::retain_external_join`] structurally never does.
    ///
    /// The shipped receive path routes an `ExternalJoinProposalMessage` to the
    /// Lakko-side quarantine and nowhere near `store_pending_proposal`, so this
    /// is the only way to reach the state where a sweep is *offered* a
    /// stranger's Add. It exists so the exclusion can be measured rather than
    /// argued from the absence of a code path.
    fn ungated_store_external_join(&mut self, identity: &Identity, bytes: &[u8]) -> Result<()> {
        let mls_in = decode_message(bytes)?;
        let protocol = mls_in
            .try_into_protocol_message()
            .map_err(|e| Error::Message(format!("not a protocol message: {e:?}")))?;
        let processed = self
            .inner
            .process_message(&identity.provider, protocol)
            .map_err(|e| Error::Message(format!("process_message: {e:?}")))?;
        let ProcessedMessageContent::ExternalJoinProposalMessage(qp) = processed.into_content()
        else {
            return Err(Error::Group("not an external join proposal".into()));
        };
        self.inner
            .store_pending_proposal(identity.provider.storage(), *qp)
            .map_err(|e| Error::Group(format!("ungated_store_external_join: {e:?}")))
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

        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
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
                account_key: Some(dave_key.clone()),
                signature_key: dave_key,
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

    // ========================================================================
    // `Update` is refused (ADR-0045 review findings F2 + F3)
    // ========================================================================

    /// How many stock `Update` proposals the flood emits. One above
    /// [`MAX_PENDING_PROPOSALS`] would do; exactly the cap is what the F3
    /// report measured, and the control below shows it is already enough.
    const FLOOD: usize = MAX_PENDING_PROPOSALS;

    /// **F3, the fix.** A plain member's `Update` proposals must never reach the
    /// pending set, however many it sends, and the owner's membership
    /// operations must be untouched afterwards.
    ///
    /// Each `Update` carries a freshly generated leaf node, so every one of them
    /// hashes to a **distinct** `ProposalRef` — which is exactly what the
    /// de-duplication in [`Group::gate_incoming_proposal`] cannot collapse, and
    /// why `Update` was the one kind able to walk a plain member's flood all the
    /// way to [`MAX_PENDING_PROPOSALS`]. The first assertion pins that the
    /// flood really is distinct, so a future change that made the wire
    /// repeat itself could not make this test pass vacuously.
    #[test]
    fn a_flood_of_stock_updates_never_reaches_the_pending_set() {
        let mut s = scene(0x30);
        let dave = Identity::from_seed(&[0x90u8; 64]).expect("dave");

        let mut refs = std::collections::BTreeSet::new();
        for i in 0..FLOOD {
            let update = s
                .charlie_g
                .ungated_propose_self_update(&s.charlie)
                .expect("charlie emits a stock Update proposal");
            refs.insert(update.clone());

            let incoming = s
                .alice_g
                .process_incoming_verifying_roles(&s.alice, &update)
                .expect("alice processes the Update");
            let Incoming::Proposal(record) = incoming else {
                panic!("expected a proposal, got {incoming:?}");
            };
            assert!(!record.stored, "Update #{i} must be refused, not retained");
            assert_eq!(
                record.kind,
                ProposalKind::Update,
                "the refusal must still name what was refused",
            );
        }
        assert_eq!(
            refs.len(),
            FLOOD,
            "the flood must be distinct on the wire, or de-duplication would \
             already stop it and this test would prove nothing",
        );
        assert!(
            s.alice_g.pending_proposals().is_empty(),
            "no Update may occupy a pending slot",
        );

        // ...and every membership operation the flood used to freeze still
        // works, on the very next call.
        s.bob_g
            .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
            .expect("an admin may still propose an add");
        s.alice_g
            .add_member_from_wire(&s.alice, &kp_wire(&dave))
            .expect("the owner may still commit an add directly");
    }

    /// **F3, the harm.** The control for the test above: had those proposals
    /// been retained, the owner's membership operations would be refused
    /// outright.
    ///
    /// The proposals are planted with [`Group::ungated_store`] — the receipt
    /// gate bypassed — so this measures the *consequence* of a full pending set
    /// rather than re-testing the gate, and it keeps measuring it whatever
    /// `authorise` later decides about `Update`. Without it, the test above
    /// would assert that a flood is refused without ever establishing that a
    /// flood matters.
    #[test]
    fn control_a_full_pending_set_freezes_the_owners_membership_operations() {
        let mut s = scene(0x31);
        let dave = Identity::from_seed(&[0x91u8; 64]).expect("dave");

        for _ in 0..FLOOD {
            let update = s
                .charlie_g
                .ungated_propose_self_update(&s.charlie)
                .expect("charlie emits a stock Update proposal");
            s.alice_g
                .ungated_store(&s.alice, &update)
                .expect("planted past the receipt gate");
        }
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            FLOOD,
            "the flood is distinct, so nothing collapses it",
        );

        assert!(
            s.alice_g
                .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
                .is_err(),
            "at the cap the owner cannot even propose",
        );
        let Err(err) = s.alice_g.add_member_from_wire(&s.alice, &kp_wire(&dave)) else {
            panic!("a direct add must be refused too");
        };
        assert!(
            format!("{err:?}").contains("membership proposal(s) are pending"),
            "expected the pending-proposal guard, got {err:?}",
        );
        assert!(s
            .alice_g
            .remove_member(&s.alice, s.charlie_g.own_leaf_index())
            .is_err());
        assert!(s
            .alice_g
            .set_group_roles(&s.alice, &RoleTable::new(pk32(&s.alice), vec![]))
            .is_err());
    }

    /// **F2.** An `Update` must never ride a sweep, so
    /// [`crate::group_proposals::owner_survives`]' blindness to one is
    /// unreachable.
    ///
    /// `commit_effects` summarises a commit's membership and role effects for
    /// `owner_survives`, and it discards `Update` — it has no field for "this
    /// leaf's signature key changed". That blindness matters because OpenMLS
    /// 0.8.1 does not bind an `Update`'s leaf node to the sender's *existing*
    /// signature key (`UpdateProposalIn::validate` verifies the leaf against the
    /// key inside it; `validate_key_uniqueness` collects signature keys from
    /// `Add` proposals only), so an `Update` can rotate its own leaf's Ed25519
    /// key out from under the role table.
    ///
    /// The fix is not to teach `commit_effects` about `Update` — it is to keep
    /// one out of every commit in the first place. This pins the two layers that
    /// do so, with the receipt gate deliberately bypassed so each is measured on
    /// its own: the sweep filter excludes it, and a peer refuses a commit that
    /// carries one anyway.
    #[test]
    fn an_update_is_excluded_from_a_sweep_and_refused_inside_a_commit() {
        let mut s = scene(0x32);

        let update = s
            .charlie_g
            .ungated_propose_self_update(&s.charlie)
            .expect("charlie emits a stock Update proposal");

        // Layer 2 — the sweep filter, over a store primed past the gate.
        s.alice_g
            .ungated_store(&s.alice, &update)
            .expect("alice stores it ungated");
        assert_eq!(s.alice_g.pending_proposals().len(), 1);
        assert!(
            s.alice_g
                .commit_pending_proposals(&s.alice)
                .expect("sweep")
                .is_none(),
            "the sweep filter must exclude an Update, so nothing is committed",
        );

        // Layer 3 — a malicious committer sweeps it unfiltered anyway.
        s.bob_g
            .ungated_store(&s.bob, &update)
            .expect("bob stores it ungated");
        let commit = s
            .bob_g
            .ungated_sweep(&s.bob)
            .expect("bob sweeps unfiltered");
        let epoch_before = s.alice_g.epoch();
        assert!(
            s.alice_g
                .process_incoming_verifying_roles(&s.alice, &wire(commit))
                .is_err(),
            "the commit gate must refuse a commit carrying an Update",
        );
        assert_eq!(
            s.alice_g.epoch(),
            epoch_before,
            "a refused commit must NOT advance the epoch",
        );
    }

    /// The refusal must be attributable. `Update` is *supported and
    /// deliberately refused*, which is a different fact from "Lakko does not
    /// know this proposal type", so folding it into
    /// [`ProposalKind::Unsupported`] would leave a developer reading
    /// "unsupported proposal type" about a proposal OpenMLS emits by default.
    #[test]
    fn the_update_refusal_names_itself() {
        let mut s = scene(0x33);
        let update = s
            .charlie_g
            .ungated_propose_self_update(&s.charlie)
            .expect("charlie emits a stock Update proposal");
        s.bob_g
            .ungated_store(&s.bob, &update)
            .expect("bob stores it ungated");
        let commit = s
            .bob_g
            .ungated_sweep(&s.bob)
            .expect("bob sweeps unfiltered");
        s.alice_g
            .ungated_store(&s.alice, &update)
            .expect("alice stores it so the by-reference lookup resolves");

        let err = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, &wire(commit))
            .expect_err("the commit must be refused");
        let text = format!("{err}");
        assert!(
            text.contains("leaf-update"),
            "the refusal must name the leaf update, got {text:?}",
        );
        assert!(
            !text.contains("unsupported"),
            "an Update is refused on purpose, not for being unrecognised: {text:?}",
        );
    }

    // ========================================================================
    // The owner's response survives a flood of admin `Add`s
    // ========================================================================

    /// Mint a fresh joiner identity, distinct for every `i`.
    fn joiner(i: usize) -> Identity {
        let mut seed = [0x90u8; 64];
        seed[0] = u8::try_from(i % 251).expect("< 256");
        seed[1] = u8::try_from(i / 251).expect("< 256");
        Identity::from_seed(&seed).expect("joiner identity")
    }

    /// Add one more plain member to a freshly built [`Scene`] through the owner's
    /// direct path (legal while nothing is pending), keeping every existing peer
    /// in sync, and return the new member's leaf.
    fn add_extra_member(s: &mut Scene, tag: u8) -> LeafNodeIndex {
        let newcomer = Identity::from_seed(&[tag; 64]).expect("newcomer identity");
        let added = s
            .alice_g
            .add_member_from_wire(&s.alice, &kp_wire(&newcomer))
            .expect("the owner adds a member");
        let commit = wire(added.commit);
        s.bob_g
            .process_incoming_verifying_roles(&s.bob, &commit)
            .expect("bob applies the add");
        s.charlie_g
            .process_incoming_verifying_roles(&s.charlie, &commit)
            .expect("charlie applies the add");
        let key = newcomer.signature_public_key().to_vec();
        let leaf = s
            .alice_g
            .members()
            .iter()
            .find(|m| m.signature_public_key == key)
            .map(|m| m.leaf_index)
            .expect("the newcomer is in the roster");
        LeafNodeIndex::new(leaf)
    }

    /// Fill `n` slots of every peer's pending set with `Add` proposals authored
    /// by the admin `bob`, and return nothing — the point is the residue.
    ///
    /// Every call here is an **ordinary, authorised** API call. No adversary
    /// seam is used and no modified client is needed: `Add` requires owner or
    /// admin, bob is an admin, and each proposal carries a distinct KeyPackage
    /// so nothing in the receipt gate's de-duplication collapses them.
    fn flood_with_admin_adds(s: &mut Scene, n: usize) {
        for i in 0..n {
            let proposal = s
                .bob_g
                .propose_add_member(&s.bob, kp(&joiner(i)))
                .unwrap_or_else(|e| panic!("an admin may propose add #{i}: {e:?}"));
            for (g, id) in [(&mut s.alice_g, &s.alice), (&mut s.charlie_g, &s.charlie)] {
                let incoming = g
                    .process_incoming_verifying_roles(id, &proposal)
                    .unwrap_or_else(|e| panic!("peer processes add #{i}: {e:?}"));
                let Incoming::Proposal(record) = incoming else {
                    panic!("expected a proposal, got {incoming:?}");
                };
                assert!(
                    record.stored,
                    "add #{i} is authorised for an admin, so it must be retained",
                );
            }
        }
    }

    /// **The escalation.** A malicious admin fills every peer's pending set with
    /// `Add` proposals; the owner must still be able to remove them and to
    /// demote them, and the sweep that enacts either must land on an honest
    /// peer whose set is equally full.
    #[test]
    fn a_flood_of_admin_adds_never_takes_away_the_owners_response() {
        let mut s = scene(0x34);
        let bob_leaf = s.bob_g.own_leaf_index();
        flood_with_admin_adds(&mut s, FLOOD);

        assert_eq!(
            s.alice_g.pending_proposals().len(),
            FLOOD,
            "the owner's pending set is full",
        );
        assert_eq!(
            s.charlie_g.pending_proposals().len(),
            FLOOD,
            "and so is an honest peer's",
        );

        // The property. Neither of these may be refused, however full the set.
        let demote = s
            .alice_g
            .propose_group_roles(&s.alice, &RoleTable::new(pk32(&s.alice), vec![]))
            .expect("the owner must still be able to demote a flooding admin");
        let remove = s
            .alice_g
            .propose_remove_member(&s.alice, bob_leaf)
            .expect("the owner must still be able to propose the flooding admin's removal");

        // An honest peer must retain both, or the sweep carrying them by
        // reference is unresolvable there (`InvalidCommit(MissingProposal)`).
        for (label, bytes) in [("demote", &demote), ("remove", &remove)] {
            let incoming = s
                .charlie_g
                .process_incoming_verifying_roles(&s.charlie, bytes)
                .unwrap_or_else(|e| panic!("charlie processes the {label}: {e:?}"));
            let Incoming::Proposal(record) = incoming else {
                panic!("expected a proposal, got {incoming:?}");
            };
            assert!(
                record.stored,
                "an honest peer must retain the owner's {label} past the cap",
            );
        }

        // And the response actually lands, on the owner and on that peer.
        let swept = s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("the owner sweeps")
            .expect("there is something to sweep");
        s.charlie_g
            .process_incoming_verifying_roles(&s.charlie, &wire(swept.commit))
            .expect("an honest peer applies the sweep");
        for (label, g) in [("owner", &s.alice_g), ("peer", &s.charlie_g)] {
            let table = g.group_roles().expect("role table");
            assert!(
                table.admins().is_empty(),
                "the flooding admin must be demoted on the {label}",
            );
            assert!(
                !g.members()
                    .iter()
                    .any(|m| m.signature_public_key == s.bob.signature_public_key()),
                "the flooding admin must be removed on the {label}",
            );
        }
    }

    /// The reserve restores an authority; it must not hand out a new one, and it
    /// must not become a general owner bypass of the cap.
    ///
    /// Three things are asserted against a full set: an **admin's** removal and
    /// role change get no reserve (the admin is the attacker in the escalation
    /// above, so a reserve it could spend would be no reserve at all); a **plain
    /// member's** departure gets none either; and the **owner's own `Add`** gets
    /// none, so the owner cannot inflate the group's bound through the one kind
    /// that is not a response.
    #[test]
    fn the_reserve_is_the_owners_alone_and_only_for_a_response() {
        let mut s = scene(0x3b);
        let charlie_leaf = s.charlie_g.own_leaf_index();
        let bob_leaf = s.bob_g.own_leaf_index();
        flood_with_admin_adds(&mut s, FLOOD);

        // An admin at the cap: refused, for a removal and for a role change
        // alike. (The role change is owner-only anyway, so the removal is the
        // load-bearing half of this pair.)
        assert!(
            s.bob_g.propose_remove_member(&s.bob, charlie_leaf).is_err(),
            "an admin gets no reserve for a removal",
        );
        assert!(
            s.bob_g
                .propose_group_roles(&s.bob, &RoleTable::new(pk32(&s.bob), vec![]))
                .is_err(),
            "an admin gets no reserve for a role change",
        );
        // A plain member leaving: refused too.
        assert!(
            s.charlie_g
                .propose_remove_member(&s.charlie, charlie_leaf)
                .is_err(),
            "a plain member gets no reserve for its own departure",
        );
        // The owner's own Add: refused. The reserve is for the response only.
        let outsider = Identity::from_seed(&[0x5cu8; 64]).expect("outsider");
        assert!(
            s.alice_g
                .propose_add_member(&s.alice, kp(&outsider))
                .is_err(),
            "the owner gets no reserve for an Add",
        );

        // And the peers' receipt gate agrees. A second scene where only the
        // owner's set was filled gives a non-owner that genuinely CAN emit a
        // proposal into a peer's full set — and that peer must drop it.
        let mut peer = scene(0x3c);
        for i in 0..FLOOD {
            let proposal = peer
                .bob_g
                .propose_add_member(&peer.bob, kp(&joiner(i)))
                .unwrap_or_else(|e| panic!("admin proposes #{i}: {e:?}"));
            peer.alice_g
                .process_incoming_verifying_roles(&peer.alice, &proposal)
                .unwrap_or_else(|e| panic!("alice processes #{i}: {e:?}"));
        }
        let departure = peer
            .charlie_g
            .propose_remove_member(&peer.charlie, peer.charlie_g.own_leaf_index())
            .expect("charlie's own set is empty, so it may ask to leave");
        let Incoming::Proposal(record) = peer
            .alice_g
            .process_incoming_verifying_roles(&peer.alice, &departure)
            .expect("alice processes it")
        else {
            panic!("expected a proposal");
        };
        assert!(
            !record.stored,
            "a plain member's proposal arriving into a full set must still be dropped",
        );

        // The owner, meanwhile, still has its response.
        s.alice_g
            .propose_remove_member(&s.alice, bob_leaf)
            .expect("the owner keeps its response");
    }

    /// Whatever the reserve admits, the set stays finite and small.
    ///
    /// The ordinary cap still refuses at [`MAX_PENDING_PROPOSALS`] and the
    /// reserve is exhaustible, so the hard ceiling is the sum and nothing can
    /// push the set past it. This is the memory bound the whole cap exists for
    /// (the set is persisted and re-parked once per outgoing chat message).
    #[test]
    fn the_pending_set_is_still_bounded_by_the_sum_of_the_two_caps() {
        assert_eq!(pending_capacity(false), MAX_PENDING_PROPOSALS);
        assert_eq!(
            pending_capacity(true),
            MAX_PENDING_PROPOSALS + MAX_RESERVED_OWNER_RESPONSES,
        );

        let mut s = scene(0x3d);
        // Enough removable leaves to spend the whole reserve on removals.
        let mut targets = vec![s.bob_g.own_leaf_index(), s.charlie_g.own_leaf_index()];
        for i in 0..MAX_RESERVED_OWNER_RESPONSES {
            targets.push(add_extra_member(
                &mut s,
                0xC0 + u8::try_from(i).expect("< 256"),
            ));
        }
        flood_with_admin_adds(&mut s, FLOOD);
        assert_eq!(s.alice_g.pending_proposals().len(), MAX_PENDING_PROPOSALS);

        for (i, leaf) in targets
            .iter()
            .take(MAX_RESERVED_OWNER_RESPONSES)
            .enumerate()
        {
            s.alice_g
                .propose_remove_member(&s.alice, *leaf)
                .unwrap_or_else(|e| {
                    panic!("the owner's response #{i} must fit the reserve: {e:?}")
                });
        }
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            MAX_PENDING_PROPOSALS + MAX_RESERVED_OWNER_RESPONSES,
            "the reserve is spent",
        );

        let err = s
            .alice_g
            .propose_remove_member(&s.alice, targets[MAX_RESERVED_OWNER_RESPONSES])
            .expect_err("the reserve is finite");
        assert!(
            format!("{err}").contains(&format!(
                "{} pending proposals",
                MAX_PENDING_PROPOSALS + MAX_RESERVED_OWNER_RESPONSES
            )),
            "the refusal must name the ceiling it enforced, got {err}",
        );
        assert!(
            s.alice_g.pending_proposals().len()
                <= MAX_PENDING_PROPOSALS + MAX_RESERVED_OWNER_RESPONSES,
            "the set must never exceed the sum of the two caps",
        );
    }

    // ========================================================================
    // A sweep must always be buildable
    // ========================================================================

    /// **One proposal used to wedge a group forever.** An `Add` naming somebody
    /// who is already a member — a stale roster, a double tap — is authorised,
    /// is retained by every peer's receipt gate, and then makes
    /// `CommitBuilder::build` fail with
    /// `ProposalValidationError::DuplicateSignatureKey` on **every** committer.
    ///
    /// The store only empties when a commit is *merged* and every direct commit
    /// is refused while proposals are pending, so an unbuildable sweep is not a
    /// lost round: it is a permanent membership wedge, and it takes the owner's
    /// removal and role change with it just as the flood does.
    #[test]
    fn a_single_add_of_an_existing_member_cannot_wedge_the_sweep() {
        let mut s = scene(0x3e);
        let readd = s
            .bob_g
            .propose_add_member(&s.bob, kp(&s.charlie))
            .expect("an admin may ask to add somebody");
        let Incoming::Proposal(record) = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, &readd)
            .expect("alice processes it")
        else {
            panic!("expected a proposal");
        };
        assert!(record.stored, "it is authorised, so it is retained");

        // The sweep must still build, and must simply carry nothing.
        assert!(
            s.alice_g
                .commit_pending_proposals(&s.alice)
                .expect("the sweep must be buildable")
                .is_none(),
            "the redundant add is the only thing pending, so nothing is committed",
        );
        // ...and the group is not wedged: membership still works afterwards.
        let dave = Identity::from_seed(&[0x5du8; 64]).expect("dave");
        s.alice_g
            .propose_add_member(&s.alice, kp(&dave))
            .expect("membership still works");
        assert!(s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("sweep")
            .is_some());
    }

    /// Two admins independently asking for the same person is the exact
    /// concurrent composition ADR-0045 exists to make safe, and it used to wedge
    /// the group: the two proposals carry different KeyPackages, so nothing
    /// de-duplicates them, and one commit may not add one signature key twice.
    #[test]
    fn two_admins_asking_for_one_person_compose_into_a_single_add() {
        let mut s = scene(0x3f);
        let dave = Identity::from_seed(&[0x5eu8; 64]).expect("dave");
        let from_bob = s
            .bob_g
            .propose_add_member(&s.bob, kp(&dave))
            .expect("bob asks for dave");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &from_bob)
            .expect("alice takes bob's request");
        s.alice_g
            .propose_add_member(&s.alice, kp(&dave))
            .expect("alice asks for dave too");
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            2,
            "distinct KeyPackages, so the receipt gate cannot collapse them",
        );

        let swept = s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("the sweep must be buildable")
            .expect("something is swept");
        assert_eq!(swept.swept.len(), 1, "exactly one of the pair is carried");
        assert!(
            s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave.signature_public_key()),
            "and dave is in the group once",
        );
        assert_eq!(s.alice_g.members().len(), 4, "alice, bob, charlie, dave");
    }

    /// The key-collision clause is judged against the roster the commit
    /// **produces**, not the one it inherits — so removing a member and re-adding
    /// the same signature key in one sweep still composes, exactly as OpenMLS's
    /// own `validate_key_uniqueness` allows.
    ///
    /// This does not discriminate the fix (it passed before it); it is the guard
    /// that keeps the fix from being written too broadly, and it fails against
    /// the obvious wrong version of clause 3 — the one that drops an `Add`
    /// colliding with *any* current member.
    #[test]
    fn a_removal_and_a_re_add_of_one_key_still_compose() {
        let mut s = scene(0x40);
        let charlie_leaf = s.charlie_g.own_leaf_index();
        s.alice_g
            .propose_remove_member(&s.alice, charlie_leaf)
            .expect("alice removes charlie");
        let readd = s
            .bob_g
            .propose_add_member(&s.bob, kp(&s.charlie))
            .expect("bob re-adds charlie");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &readd)
            .expect("alice takes the re-add");

        let swept = s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("sweep")
            .expect("something is swept");
        assert_eq!(
            swept.swept.len(),
            2,
            "the removal and the re-add must BOTH be carried",
        );
        assert!(
            s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == s.charlie.signature_public_key()),
            "charlie is back in the group under a fresh leaf",
        );
    }

    /// Excluding a duplicate `Add` must not change what
    /// [`crate::group_proposals::owner_survives`] concluded, which is computed
    /// over the authorised set **before** the filter runs.
    ///
    /// The case that would break it: an ownership transfer whose named owner is
    /// only reachable through an `Add` the filter then drops. It cannot happen —
    /// a key dropped as a duplicate is still added by the copy that was kept, and
    /// a key dropped for colliding with a member is a member the commit keeps —
    /// and this pins the second half, which is the one an over-eager clause 3
    /// would break.
    #[test]
    fn an_excluded_duplicate_add_does_not_change_the_owner_survives_answer() {
        let mut s = scene(0x41);
        // A redundant Add of charlie, who is already a member and is about to be
        // named owner.
        let readd = s
            .bob_g
            .propose_add_member(&s.bob, kp(&s.charlie))
            .expect("bob re-adds charlie");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &readd)
            .expect("alice takes it");
        s.alice_g
            .propose_ownership_transfer(
                &s.alice,
                s.charlie.signature_public_key(),
                OutgoingOwner::Admin,
            )
            .expect("alice hands the group to charlie");

        let swept = s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("the sweep must be buildable")
            .expect("something is swept");
        assert_eq!(
            swept.swept.len(),
            1,
            "only the transfer is carried; the redundant add is dropped",
        );
        let table = s.alice_g.group_roles().expect("role table");
        assert_eq!(
            table.owner().as_slice(),
            s.charlie.signature_public_key(),
            "the transfer landed, so owner_survives said yes and meant it",
        );
    }

    /// The cheapest form of the flood: every `Add` names ONE person, so nothing
    /// de-duplicates the proposals and the old sweep could not be built at all.
    /// The owner's response must survive it, and the sweep must land.
    #[test]
    fn a_same_key_flood_neither_wedges_the_sweep_nor_silences_the_owner() {
        let mut s = scene(0x42);
        let bob_leaf = s.bob_g.own_leaf_index();
        let dave = Identity::from_seed(&[0x5fu8; 64]).expect("dave");
        for i in 0..FLOOD {
            let proposal = s
                .bob_g
                .propose_add_member(&s.bob, kp(&dave))
                .unwrap_or_else(|e| panic!("admin proposes #{i}: {e:?}"));
            s.alice_g
                .process_incoming_verifying_roles(&s.alice, &proposal)
                .unwrap_or_else(|e| panic!("alice processes #{i}: {e:?}"));
        }
        assert_eq!(s.alice_g.pending_proposals().len(), FLOOD);

        s.alice_g
            .propose_remove_member(&s.alice, bob_leaf)
            .expect("the owner must still be able to respond");
        let swept = s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("the sweep must be buildable")
            .expect("something is swept");
        assert_eq!(
            swept.swept.len(),
            2,
            "one add out of the whole flood, plus the owner's removal",
        );
        assert!(
            !s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == s.bob.signature_public_key()),
            "the flooding admin is gone",
        );
    }

    // ------------------------------------------------------------------
    // ...and the second route to the same wedge: a leaf that cannot live
    // in THIS group (`sweep_keeps` clauses 4 and 5).
    //
    // Every KeyPackage below is well-formed — each is built by OpenMLS's own
    // `KeyPackage::builder()` and passes `KeyPackageIn::validate` — and each
    // makes `CommitBuilder::build` refuse outright, because "may this leaf join
    // THAT group" is a question only the group can ask and nothing on the way
    // in asks it.
    // ------------------------------------------------------------------

    /// Build a [`SweepContext`] over a real group, so the clause-4 predicate can
    /// be exercised directly for the conditions the wire cannot reach.
    ///
    /// Everything it holds is read from the group exactly as
    /// [`Group::stage_pending_proposals`] reads it; only the entry point differs.
    fn admission_ctx<'a>(
        identity: &'a Identity,
        table: &'a RoleTable,
        members: &'a [MemberRecord],
        admission: &'a GroupAdmission,
    ) -> SweepContext<'a> {
        static NO_KIND: std::sync::OnceLock<KindState> = std::sync::OnceLock::new();
        SweepContext {
            table,
            members,
            kind_before: NO_KIND.get_or_init(KindState::default),
            role_change_survives: true,
            removed_leaves: &[],
            admission,
            identity,
        }
    }

    /// **The headline, and it needs no adversary.** An `Add` whose leaf does not
    /// advertise the group's own ciphersuite is authorised by
    /// [`crate::group_proposals::authorise`], is accepted by OpenMLS's own
    /// proposer-side check, is retained by every peer's receipt gate — and then
    /// makes `CommitBuilder::build` fail with
    /// `LeafNodeValidationError::CiphersuiteNotInCapabilities` on **every**
    /// committer.
    ///
    /// The store only empties when a commit is *merged* and every direct commit
    /// is refused while proposals are pending, so an unbuildable sweep is not a
    /// lost round: it is a permanent membership wedge, invisible because chat
    /// keeps working over it.
    #[test]
    fn a_capability_deficient_add_cannot_wedge_the_sweep() {
        let mut s = scene(0x44);
        let dave = Identity::from_seed(&[0x60u8; 64]).expect("dave");
        let deficient = dave
            .key_package_without_ciphersuite_capability()
            .expect("a conformant client may mint this");

        let proposal = s
            .bob_g
            .propose_add_member(&s.bob, deficient)
            .expect("an admin may ask to add somebody");
        let Incoming::Proposal(record) = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, &proposal)
            .expect("alice processes it")
        else {
            panic!("expected a proposal");
        };
        assert!(
            record.stored,
            "it is authorised, so it is retained — the exclusion is not a refusal",
        );

        assert!(
            s.alice_g
                .commit_pending_proposals(&s.alice)
                .expect("the sweep must be buildable")
                .is_none(),
            "the uncommittable add is the only thing pending, so nothing is committed",
        );
        assert!(
            !s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave.signature_public_key()),
            "and dave did not join",
        );

        // ...and the group is not wedged: membership still works afterwards.
        let erin = Identity::from_seed(&[0x61u8; 64]).expect("erin");
        s.alice_g
            .propose_add_member(&s.alice, kp(&erin))
            .expect("membership still works");
        assert!(s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("sweep")
            .is_some());
    }

    /// The same wedge through the credential dimension: a leaf advertising no
    /// credential type satisfies neither its own credential (`validate_locally`)
    /// nor the members' (valn0104), and `build` refuses with
    /// `LeafNodeValidationError::UnsupportedCredentials`.
    #[test]
    fn an_add_whose_leaf_supports_no_credential_type_cannot_wedge_the_sweep() {
        let mut s = scene(0x46);
        let dave = Identity::from_seed(&[0x64u8; 64]).expect("dave");
        let proposal = s
            .bob_g
            .propose_add_member(
                &s.bob,
                dave.key_package_without_credential_capability()
                    .expect("deficient kp"),
            )
            .expect("an admin may ask to add somebody");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &proposal)
            .expect("alice takes it");

        assert!(
            s.alice_g
                .commit_pending_proposals(&s.alice)
                .expect("the sweep must be buildable")
                .is_none(),
            "nothing committable is pending",
        );
    }

    /// **Time alone reaches this one.** A KeyPackage's lifetime is checked at
    /// `CommitBuilder::build`, and ADR-0045 gives a request no deadline — so a
    /// proposal can outlive the KeyPackage it names and wedge every sweep from
    /// then on, with nobody having done anything wrong.
    ///
    /// It is also the one clause-4 condition that cannot be decided once and
    /// cached: the answer changes while the proposal sits in the store, which is
    /// why the check belongs in the filter and not in the receipt gate.
    #[test]
    fn an_add_whose_key_package_has_expired_cannot_wedge_the_sweep() {
        let mut s = scene(0x47);
        let dave = Identity::from_seed(&[0x65u8; 64]).expect("dave");
        s.alice_g
            .propose_add_member(&s.alice, dave.expired_key_package().expect("expired kp"))
            .expect("the owner may ask to add somebody");
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            1,
            "the expired add is retained, as any authorised proposal is",
        );

        assert!(
            s.alice_g
                .commit_pending_proposals(&s.alice)
                .expect("the sweep must be buildable")
                .is_none(),
            "nothing committable is pending",
        );
        assert!(
            !s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave.signature_public_key()),
            "and dave did not join on an expired key package",
        );
    }

    /// An uncommittable `Add` must not take the rest of the sweep with it: the
    /// good add and the owner's removal in the same pending set both land.
    #[test]
    fn a_capability_deficient_add_does_not_take_the_sweep_with_it() {
        let mut s = scene(0x45);
        let dave = Identity::from_seed(&[0x62u8; 64]).expect("dave");
        let erin = Identity::from_seed(&[0x63u8; 64]).expect("erin");
        let charlie_leaf = s.charlie_g.own_leaf_index();

        let bad = s
            .bob_g
            .propose_add_member(
                &s.bob,
                dave.key_package_without_ciphersuite_capability()
                    .expect("deficient kp"),
            )
            .expect("bob asks for dave");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &bad)
            .expect("alice takes it");
        s.alice_g
            .propose_add_member(&s.alice, kp(&erin))
            .expect("alice asks for erin");
        s.alice_g
            .propose_remove_member(&s.alice, charlie_leaf)
            .expect("alice removes charlie");

        let swept = s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("the sweep must be buildable")
            .expect("something is swept");
        assert_eq!(
            swept.swept.len(),
            2,
            "erin's add and charlie's removal, but not dave's",
        );
        assert!(
            s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == erin.signature_public_key()),
            "erin joined",
        );
        assert!(
            !s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == s.charlie.signature_public_key()),
            "charlie is gone",
        );
    }

    /// **The condition ADR-0045's recon named, pinned one level down.** A leaf
    /// that does not advertise the `0xFF02` role-table extension the GroupContext
    /// carries makes `validate_add_proposals` fail with
    /// `InsufficientCapabilities`.
    ///
    /// It is exercised through [`added_leaf_can_join`] and [`sweep_keeps`]
    /// rather than through a live proposal, and the reason is worth recording
    /// rather than working around: OpenMLS's own `create_add_proposal` refuses
    /// to **emit** this proposal, because a Lakko group lists `0xFF02` in
    /// `required_capabilities` and the proposer checks that one thing. So the
    /// only way it reaches a store is a peer whose MLS stack does not run that
    /// check — and a proposal is signed over the group context, so no
    /// in-process test can forge one from outside. **What is untested is the
    /// journey, not the verdict**, and the verdict is asserted at the same
    /// function the sweep calls.
    ///
    /// The clause is not thereby redundant with the `required_capabilities`
    /// clause it coincides with today: they coincide only because every
    /// GroupContext extension [`Group::create`] installs is also declared
    /// required. A future extension installed without that declaration would
    /// separate them, silently, in the wedging direction.
    #[test]
    fn a_leaf_that_cannot_carry_the_group_context_extensions_is_not_committable() {
        let s = scene(0x48);
        let dave = Identity::from_seed(&[0x66u8; 64]).expect("dave");
        let table = s.alice_g.group_roles().expect("role table");
        let members = s.alice_g.members();
        let admission = s.alice_g.admission_requirements(&[]);
        let ctx = admission_ctx(&s.alice, &table, &members, &admission);

        assert!(
            !added_leaf_can_join(
                &ctx,
                &dave
                    .key_package_without_extension_capabilities()
                    .expect("deficient kp"),
            ),
            "a leaf that cannot carry 0xFF02 must never be committed into a role-enabled group",
        );
        assert!(
            added_leaf_can_join(&ctx, &kp(&dave)),
            "...and an ordinary Lakko KeyPackage must be",
        );

        // And the same verdict through the filter itself, on a genuine
        // `QueuedProposal`. The proposal is minted in a table-less group
        // (`create_with_id` declares no `required_capabilities`, so OpenMLS's
        // proposer-side check has nothing to refuse it with) and then judged
        // against the role-enabled group's context — which is exactly the
        // asymmetry a peer running a different MLS stack would create.
        let mut open_group = Group::create_with_id(&s.alice, &[0x48u8; GROUP_ID_LEN])
            .expect("a group with no role table and no required capabilities");
        open_group
            .ungated_propose_add(
                &s.alice,
                dave.key_package_without_extension_capabilities()
                    .expect("deficient kp"),
            )
            .expect("nothing in that group refuses it");
        let queued: Vec<_> = open_group.inner.pending_proposals().cloned().collect();
        let qp = queued.first().expect("the deficient add is pending");
        let mut state = SweepState::default();
        assert!(
            !sweep_keeps(&ctx, &mut state, qp),
            "the sweep filter must exclude it, not merely the predicate",
        );
    }

    /// The good path, asserted rather than assumed: an ordinary `Add` of a
    /// properly-minted member is untouched by clauses 4 and 5.
    #[test]
    fn an_ordinary_add_of_a_properly_minted_member_still_commits() {
        let mut s = scene(0x49);
        let dave = Identity::from_seed(&[0x67u8; 64]).expect("dave");
        let proposal = s
            .bob_g
            .propose_add_member(&s.bob, kp(&dave))
            .expect("bob asks for dave");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &proposal)
            .expect("alice takes it");

        let swept = s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("sweep")
            .expect("something is swept");
        assert_eq!(swept.swept.len(), 1);
        assert!(
            s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave.signature_public_key()),
            "dave joined",
        );
        assert_eq!(s.alice_g.members().len(), 4);
    }

    /// **Clause 4 decides what is committable; it must not touch who may ask.**
    ///
    /// The same deficient KeyPackage is proposed by an admin and by a plain
    /// member: the admin's is *stored* and then excluded at sweep time, the
    /// plain member's is *refused at the receipt gate*. Both end with nothing
    /// committed, and the two must stay distinguishable — a filter that started
    /// answering the authorisation question would collapse them.
    #[test]
    fn excluding_an_uncommittable_add_did_not_widen_or_narrow_authorisation() {
        let mut s = scene(0x4a);
        let dave = Identity::from_seed(&[0x68u8; 64]).expect("dave");

        // The admin's ask: authorised, therefore stored.
        let from_admin = s
            .bob_g
            .propose_add_member(
                &s.bob,
                dave.key_package_without_ciphersuite_capability()
                    .expect("deficient kp"),
            )
            .expect("an admin may ask");
        let Incoming::Proposal(admin_record) = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, &from_admin)
            .expect("alice processes it")
        else {
            panic!("expected a proposal");
        };
        assert!(
            admin_record.stored,
            "clause 4 excludes at sweep time; it must not turn an authorised ask into a refusal",
        );

        // The plain member's identical ask: still refused, unstored.
        let hostile = s
            .charlie_g
            .ungated_propose_add(
                &s.charlie,
                dave.key_package_without_ciphersuite_capability()
                    .expect("deficient kp"),
            )
            .expect("charlie proposes without its local check");
        let Incoming::Proposal(member_record) = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, &hostile)
            .expect("alice processes it")
        else {
            panic!("expected a proposal");
        };
        assert!(
            !member_record.stored,
            "a plain member's Add is still refused on authority, exactly as before",
        );
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            1,
            "only the admin's ask is held",
        );
    }

    /// Clause 5, isolated. Two `Add`s carrying the same HPKE encryption key
    /// under **different** signature keys slip past clause 3 and make
    /// `validate_key_uniqueness` refuse the build with `DuplicateEncryptionKey`
    /// — the same wedge through a different key.
    ///
    /// No KeyPackage minted through `KeyPackage::builder()` can collide this
    /// way, because the builder generates both keys itself; the collision is
    /// reachable only from an MLS stack that is not OpenMLS. So the clause is
    /// pinned where it decides: a pass that has already taken one leaf's
    /// encryption key must refuse the next proposal presenting it, in either
    /// direction, and whether the key was taken by a kept add or by a sitting
    /// member.
    #[test]
    fn a_key_already_serving_the_group_makes_an_add_uncommittable() {
        let mut s = scene(0x4b);
        let dave = Identity::from_seed(&[0x69u8; 64]).expect("dave");
        s.alice_g
            .propose_add_member(&s.alice, kp(&dave))
            .expect("the owner may ask for dave");
        let table = s.alice_g.group_roles().expect("role table");
        let members = s.alice_g.members();
        let admission = s.alice_g.admission_requirements(&[]);
        let queued: Vec<_> = s.alice_g.inner.pending_proposals().cloned().collect();
        let qp = queued.first().expect("dave's add is pending");
        let encryption_key = match qp.proposal() {
            Proposal::Add(add) => add.key_package().leaf_node().encryption_key().clone(),
            other => panic!("expected an Add, got {other:?}"),
        };
        let init_key = match qp.proposal() {
            Proposal::Add(add) => {
                EncryptionKey::from(add.key_package().hpke_init_key().as_slice().to_vec())
            },
            other => panic!("expected an Add, got {other:?}"),
        };

        // Baseline: on a fresh pass it is kept, and the pass records both keys.
        let ctx = admission_ctx(&s.alice, &table, &members, &admission);
        let mut fresh = SweepState::default();
        assert!(sweep_keeps(&ctx, &mut fresh, qp), "the good add is kept");
        assert!(fresh.kept_encryption_keys.contains(&encryption_key));
        assert!(fresh.kept_init_keys.contains(&init_key));

        // A pass that already took this encryption key must refuse it, even
        // though clause 3 has nothing to say (the signature key is untaken).
        let mut enc_taken = SweepState {
            kept_encryption_keys: vec![encryption_key.clone()],
            ..SweepState::default()
        };
        assert!(!sweep_keeps(&ctx, &mut enc_taken, qp));

        // The cross direction: an earlier add's INIT key is this add's
        // encryption key.
        let mut init_taken = SweepState {
            kept_init_keys: vec![encryption_key.clone()],
            ..SweepState::default()
        };
        assert!(!sweep_keeps(&ctx, &mut init_taken, qp));

        // And a key already held by a member the commit keeps.
        let seeded = GroupAdmission {
            staying_encryption_keys: vec![encryption_key],
            ..admission
        };
        let member_ctx = admission_ctx(&s.alice, &table, &members, &seeded);
        let mut pass = SweepState::default();
        assert!(!sweep_keeps(&member_ctx, &mut pass, qp));
    }

    /// A `Leave` cannot fill the set: `SelfRemove` names no target, so one member
    /// has exactly one of them to give and the receipt gate's de-duplication
    /// collapses every repeat.
    #[test]
    fn a_member_cannot_fill_the_set_by_leaving_repeatedly() {
        let mut s = scene(0x43);
        for _ in 0..FLOOD {
            let leave = s
                .charlie_g
                .propose_remove_member(&s.charlie, s.charlie_g.own_leaf_index())
                .expect("a member may always ask to leave");
            let Incoming::Proposal(record) = s
                .alice_g
                .process_incoming_verifying_roles(&s.alice, &leave)
                .expect("alice processes it")
            else {
                panic!("expected a proposal");
            };
            assert!(
                record.stored,
                "the request is held (it is the same request)"
            );
        }
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            1,
            "a repeated departure occupies exactly one slot",
        );
    }

    // ========================================================================
    // The last-resort exclusion fallback
    //
    // `sweep_keeps` enumerates the causes of an unbuildable sweep that somebody
    // has thought of. The tests below are about the ones nobody has: a build
    // that fails AFTER the enumeration has had its say. The enumeration is
    // written against a rule set this crate does not own, and one of the rules
    // it restates — a KeyPackage's lifetime — fires with no adversary at all,
    // so "we have listed them" can never be the whole answer.
    // ========================================================================

    /// **The headline, and it needs neither an adversary nor a seam.**
    ///
    /// An admin asking to leave is ordinary ADR-0045 traffic — the ADR made
    /// leaving reachable precisely as "the member proposes it and an admin
    /// commits". But OpenMLS refuses to let a committer remove its own leaf
    /// (`CreateCommitError::CannotRemoveSelf`, raised inside
    /// `CommitBuilder::build` after the proposals have been applied to the
    /// diff), so the departing admin's OWN sweep fails — and `sweep_keeps` does
    /// not enumerate that, because the proposal is not defective at all: it is
    /// perfectly committable by anybody else.
    ///
    /// Before the fallback this returned `Err("sweep build: CannotRemoveSelf")`,
    /// and since a pending proposal also closes every direct-commit path
    /// ([`Group::refuse_if_proposals_pending`]), that device's membership
    /// operations were frozen for as long as it was the acting committer — with
    /// chat still working over the top of it.
    #[test]
    fn a_departing_admins_own_sweep_still_commits_the_rest() {
        let mut s = scene(0x50);
        let dave = Identity::from_seed(&[0x6au8; 64]).expect("dave");
        let bob_leaf = s.bob_g.own_leaf_index();

        s.bob_g
            .propose_remove_member(&s.bob, bob_leaf)
            .expect("an admin may always ask to leave");
        s.bob_g
            .propose_add_member(&s.bob, kp(&dave))
            .expect("an admin may ask to add somebody");
        assert_eq!(s.bob_g.pending_proposals().len(), 2);

        let swept = s
            .bob_g
            .commit_pending_proposals(&s.bob)
            .expect("the departing admin's own sweep must still be buildable")
            .expect("something is swept");
        assert_eq!(
            swept.swept.len(),
            1,
            "dave's add lands; bob's own departure is not committable BY BOB",
        );
        assert!(
            s.bob_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave.signature_public_key()),
            "dave joined",
        );
        assert!(
            s.bob_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == s.bob.signature_public_key()),
            "and bob is still a member — the exclusion did not enact its departure",
        );
        assert!(s.bob_g.is_active(), "bob's own group is untouched and live");
    }

    /// The good path must be exactly as cheap as it was: one `CommitBuilder`
    /// build, no probes. The fallback is reached only through a failure, so an
    /// ordinary sweep must never pay for it.
    #[test]
    fn an_ordinary_sweep_costs_exactly_one_build() {
        let mut s = scene(0x51);
        let dave = Identity::from_seed(&[0x6bu8; 64]).expect("dave");
        s.alice_g
            .propose_add_member(&s.alice, kp(&dave))
            .expect("the owner may ask to add somebody");

        reset_sweep_builds();
        let swept = s
            .alice_g
            .commit_pending_proposals(&s.alice)
            .expect("sweep")
            .expect("something is swept");
        assert_eq!(swept.swept.len(), 1);
        assert_eq!(
            sweep_builds(),
            1,
            "an ordinary sweep must build once and probe not at all",
        );
    }

    /// **The seam.** [`Group::sweep_with_authorisation_only_filter`] is the
    /// enumeration reduced to its first clause — authorisation, and nothing
    /// else — which is exactly what `sweep_keeps` was before clauses 2-5 were
    /// written. It stands in for a cause nobody has enumerated: the proposal
    /// below IS caught by clause 3 today, and blinding the filter to it is how
    /// a condition the filter cannot see is reproduced without inventing a
    /// hypothetical one.
    ///
    /// What this proves: given a selection the enumeration passed and the
    /// builder refuses, the fallback finds the offender by exclusion and lands
    /// the rest. What it does not prove: that any *particular* unenumerated
    /// cause exists — [`a_departing_admins_own_sweep_still_commits_the_rest`] is
    /// the live one.
    #[test]
    fn the_fallback_rescues_a_wedge_the_enumeration_cannot_see() {
        let mut s = scene(0x52);
        let dave = Identity::from_seed(&[0x6cu8; 64]).expect("dave");
        let readd = s
            .bob_g
            .propose_add_member(&s.bob, kp(&s.charlie))
            .expect("bob re-adds charlie, who is already a member");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &readd)
            .expect("alice takes it");
        s.alice_g
            .propose_add_member(&s.alice, kp(&dave))
            .expect("alice asks for dave");

        let swept = s
            .alice_g
            .sweep_with_authorisation_only_filter(&s.alice)
            .expect("the blind sweep must still be buildable")
            .expect("something is swept");
        assert_eq!(
            swept.swept.len(),
            1,
            "dave's add lands; the duplicate is found by exclusion",
        );
        assert!(
            s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave.signature_public_key()),
            "dave joined",
        );
        assert_eq!(s.alice_g.members().len(), 4, "and charlie is in it once");
    }

    /// Several independent offenders in one pending set: each is excluded, and
    /// the one good proposal still lands. Leave-one-out would need a second
    /// round per offender; the additive search pays one probe per candidate
    /// whether there is one offender or all of them.
    #[test]
    fn several_offenders_are_all_excluded_and_the_rest_commits() {
        let mut s = scene(0x53);
        let dave = Identity::from_seed(&[0x6du8; 64]).expect("dave");
        for _ in 0..3 {
            let readd = s
                .bob_g
                .propose_add_member(&s.bob, kp(&s.charlie))
                .expect("bob re-adds charlie again");
            s.alice_g
                .process_incoming_verifying_roles(&s.alice, &readd)
                .expect("alice takes it");
        }
        s.alice_g
            .propose_add_member(&s.alice, kp(&dave))
            .expect("alice asks for dave");
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            4,
            "three distinct KeyPackages plus the good add",
        );

        let swept = s
            .alice_g
            .sweep_with_authorisation_only_filter(&s.alice)
            .expect("the blind sweep must still be buildable")
            .expect("something is swept");
        assert_eq!(swept.swept.len(), 1, "only dave's add survives");
        assert_eq!(s.alice_g.members().len(), 4);
    }

    /// Every proposal uncommittable. The honest outcome is **no commit at all**
    /// — never an epoch-advancing empty one — which is byte-for-byte what the
    /// enumeration already does when its filter keeps nothing.
    ///
    /// The pending set therefore survives, exactly as it does today when clause
    /// 3 drops everything: the store empties only on a merged commit. That is
    /// not a wedge, and the second half proves it — one committable proposal
    /// later, a commit lands and takes the whole set with it.
    #[test]
    fn a_wholly_uncommittable_set_yields_no_commit_and_no_wedge() {
        let mut s = scene(0x54);
        for _ in 0..2 {
            let readd = s
                .bob_g
                .propose_add_member(&s.bob, kp(&s.charlie))
                .expect("bob re-adds charlie");
            s.alice_g
                .process_incoming_verifying_roles(&s.alice, &readd)
                .expect("alice takes it");
        }
        let epoch_before = s.alice_g.epoch();

        assert!(
            s.alice_g
                .sweep_with_authorisation_only_filter(&s.alice)
                .expect("the blind sweep must still be buildable")
                .is_none(),
            "nothing is committable, so nothing is committed",
        );
        assert_eq!(s.alice_g.epoch(), epoch_before, "and no epoch was burned");
        assert!(!s.alice_g.has_staged_sweep());
        assert_eq!(
            s.alice_g.pending_proposals().len(),
            2,
            "the store empties only on a merged commit",
        );

        // ...and the group is not wedged: one committable proposal later, a
        // commit lands and clears the whole set.
        let erin = Identity::from_seed(&[0x6eu8; 64]).expect("erin");
        s.alice_g
            .propose_add_member(&s.alice, kp(&erin))
            .expect("alice asks for erin");
        let swept = s
            .alice_g
            .sweep_with_authorisation_only_filter(&s.alice)
            .expect("sweep")
            .expect("something is swept");
        assert_eq!(swept.swept.len(), 1, "erin's add, and only it");
        assert!(s.alice_g.pending_proposals().is_empty());
    }

    /// **The property the whole design rests on**: `CommitBuilder::build`
    /// validates before it takes `public_group.empty_diff()`, holds the group by
    /// shared reference throughout, and writes nothing — `stage_commit` is the
    /// writing half. So a probe is free of consequences whichever way it
    /// answers, which is what makes an exclusion search legitimate at all.
    ///
    /// Both answers are asserted, because only the *successful* one could
    /// plausibly leave something behind.
    #[test]
    fn a_probe_leaves_the_group_and_the_store_untouched_either_way() {
        let mut s = scene(0x55);
        let dave = Identity::from_seed(&[0x71u8; 64]).expect("dave");
        let bob_leaf = s.bob_g.own_leaf_index();
        s.bob_g
            .propose_remove_member(&s.bob, bob_leaf)
            .expect("an admin may ask to leave");
        s.bob_g
            .propose_add_member(&s.bob, kp(&dave))
            .expect("an admin may ask to add somebody");

        let all: Vec<_> = s
            .bob_g
            .inner
            .pending_proposals()
            .map(|qp| qp.proposal_reference_ref().clone())
            .collect();
        assert_eq!(all.len(), 2);
        let epoch_before = s.bob_g.epoch();
        let members_before = s.bob_g.members().len();

        // The refusing probe: a committer cannot remove its own leaf.
        assert!(
            !s.bob_g.sweep_selection_builds(&s.bob, &all),
            "the whole set does not build",
        );
        // ...and the succeeding one: dave's add alone is perfectly buildable.
        assert!(
            s.bob_g.sweep_selection_builds(&s.bob, &all[1..]),
            "dave's add alone does build",
        );

        assert_eq!(s.bob_g.epoch(), epoch_before, "the epoch did not move");
        assert_eq!(
            s.bob_g.members().len(),
            members_before,
            "the roster did not move",
        );
        assert_eq!(
            s.bob_g.pending_proposals().len(),
            2,
            "the proposal store did not move",
        );
        assert!(
            !s.bob_g.has_staged_sweep(),
            "and nothing was staged behind our back",
        );
        assert!(
            !s.bob_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave.signature_public_key()),
            "least of all did the successful probe let dave in",
        );
    }

    /// The exclusion search must never assemble a selection that leaves the
    /// group with an owner who is not in it. `owner_survives` is a property of
    /// the commit as a WHOLE, so dropping an `Add` can strand a role change the
    /// enumeration had already blessed — and OpenMLS, which knows nothing about
    /// role tables, will happily build the stranded commit.
    ///
    /// The setup needs the ungated proposer because an honest client refuses to
    /// emit a table naming a non-member (`refuse_unenforceable_role_table`); a
    /// modified one can, and the receipt gate does not ask. A search that
    /// judged nothing but "does it build" would keep the transfer, publish it,
    /// and have every peer refuse the commit ([`authorise_role_commit`]) —
    /// trading a local wedge for a fork.
    #[test]
    fn the_exclusion_search_never_leaves_the_group_without_an_owner() {
        let mut s = scene(0x56);
        let dave = Identity::from_seed(&[0x6fu8; 64]).expect("dave");
        s.alice_g
            .ungated_propose_group_roles(&s.alice, &RoleTable::new(pk32(&dave), vec![]))
            .expect("a modified owner names dave, who is not in the group yet");
        s.alice_g
            .propose_add_member(
                &s.alice,
                dave.key_package_without_ciphersuite_capability()
                    .expect("a conformant client may mint this"),
            )
            .expect("...and asks for dave in the same round");

        let outcome = s
            .alice_g
            .sweep_with_authorisation_only_filter(&s.alice)
            .expect("the blind sweep must still be buildable");
        assert!(
            outcome.is_none(),
            "dave's add is uncommittable, so the role change naming dave cannot ride alone",
        );
        let table = s.alice_g.group_roles().expect("role table");
        assert_eq!(
            table.owner(),
            s.alice.signature_public_key(),
            "alice is still the owner",
        );
        assert!(
            s.alice_g.group_roles().is_some(),
            "and the group still has a role table at all",
        );
    }

    /// **The second pass earns its keep.** Two proposals that are committable
    /// only *together* — a `Remove` and the re-`Add` of the same signature key
    /// — must both survive the search even when the store offers the `Add`
    /// first, at which point it collides with a member the selection does not
    /// yet remove.
    ///
    /// One additive pass alone would drop the re-add and quietly evict charlie
    /// from the group; re-offering every rejected candidate against the
    /// finished selection recovers it. This is the same shape as an ownership
    /// transfer arriving ahead of the `Add` that puts its owner in the group.
    #[test]
    fn a_pair_that_is_only_committable_together_survives_the_wrong_store_order() {
        let mut s = scene(0x58);
        let dave = Identity::from_seed(&[0x70u8; 64]).expect("dave");
        let charlie_leaf = s.charlie_g.own_leaf_index();

        // Deliberately the awkward order: the re-add lands in the store BEFORE
        // the removal that makes it committable.
        s.alice_g
            .propose_add_member(&s.alice, kp(&s.charlie))
            .expect("alice re-adds charlie");
        s.alice_g
            .propose_remove_member(&s.alice, charlie_leaf)
            .expect("alice removes charlie");
        // ...and one proposal the builder will refuse, so the fallback runs at
        // all.
        s.alice_g
            .propose_add_member(
                &s.alice,
                dave.key_package_without_ciphersuite_capability()
                    .expect("a conformant client may mint this"),
            )
            .expect("alice asks for dave");

        let swept = s
            .alice_g
            .sweep_with_authorisation_only_filter(&s.alice)
            .expect("the blind sweep must still be buildable")
            .expect("something is swept");
        assert_eq!(
            swept.swept.len(),
            2,
            "the removal AND the re-add; only dave's add is excluded",
        );
        assert!(
            s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == s.charlie.signature_public_key()),
            "charlie is back under a fresh leaf, not evicted by the search",
        );
        assert!(
            !s.alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave.signature_public_key()),
            "and dave did not join on an unusable KeyPackage",
        );
    }

    /// The search is **deterministic** on one device: same store, same order,
    /// same answer, however many times it is asked. Nothing about the number of
    /// attempts, and nothing random, enters the choice.
    ///
    /// It is deliberately NOT asserted to be device-INDEPENDENT, because it is
    /// not: [`a_departing_admins_own_sweep_still_commits_the_rest`] is a case
    /// where the answer legitimately differs between committers, since "can I
    /// commit this" is a question about the committer. Two committers can
    /// therefore stage different subsets of one pending set — which ADR-0045
    /// already arbitrates: the DS journal orders the two commits, the loser
    /// abandons its staged sweep, and whichever landed is what every member
    /// applies. Unpredictable, never divergent.
    #[test]
    fn the_exclusion_search_is_deterministic_for_one_committer() {
        let mut s = scene(0x5a);
        let dave = Identity::from_seed(&[0x72u8; 64]).expect("dave");
        let erin = Identity::from_seed(&[0x73u8; 64]).expect("erin");
        for _ in 0..2 {
            s.alice_g
                .propose_add_member(&s.alice, kp(&s.charlie))
                .expect("alice re-adds charlie");
        }
        s.alice_g
            .propose_add_member(&s.alice, kp(&dave))
            .expect("alice asks for dave");
        s.alice_g
            .propose_add_member(&s.alice, kp(&erin))
            .expect("alice asks for erin");

        let table = s.alice_g.group_roles().expect("role table");
        let members = s.alice_g.members();
        let queued: Vec<QueuedProposal> = s.alice_g.inner.pending_proposals().cloned().collect();
        let candidates: Vec<ProposalRef> = queued
            .iter()
            .filter(|qp| proposal_is_authorised(&table, &members, qp))
            .map(|qp| qp.proposal_reference_ref().clone())
            .collect();
        assert_eq!(candidates.len(), 4);

        let first = s
            .alice_g
            .narrow_sweep_by_exclusion(&s.alice, &queued, &members, &candidates);
        let second = s
            .alice_g
            .narrow_sweep_by_exclusion(&s.alice, &queued, &members, &candidates);
        assert_eq!(first.len(), 2, "dave and erin; neither re-add of charlie");
        assert_eq!(first, second, "the search is a pure function of its inputs");
    }

    /// **One bad apple must not cost the barrel.** A pending set at the cap in
    /// which every proposal but one is committable: the search must land all
    /// sixty-three and exclude only the one the builder refuses.
    ///
    /// This is also where the search is at its most **expensive**, and the cost
    /// is not the candidate count alone: every probe carries the selection
    /// accepted so far, so a build here computes a path and encrypts a Welcome
    /// for up to sixty-three joiners. Measured on this test at the time of
    /// writing: **67 builds, ~2.2 s in a debug build and ~215 ms in release**,
    /// against ~8 ms for the 129-build all-uncommittable case below, where
    /// nothing is ever accepted and every probe therefore carries one proposal.
    /// Paid once, on a sweep that would otherwise have bricked the group's
    /// membership permanently.
    #[test]
    fn one_uncommittable_proposal_in_a_full_set_costs_only_itself() {
        let mut s = scene(0x59);
        let dave = Identity::from_seed(&[0x7fu8; 64]).expect("dave");
        for i in 0..(MAX_PENDING_PROPOSALS - 1) {
            s.bob_g
                .propose_add_member(&s.bob, kp(&joiner(i)))
                .unwrap_or_else(|e| panic!("an admin may propose add #{i}: {e:?}"));
        }
        s.bob_g
            .propose_add_member(
                &s.bob,
                dave.key_package_without_ciphersuite_capability()
                    .expect("a conformant client may mint this"),
            )
            .expect("...and one the builder will refuse");
        assert_eq!(s.bob_g.pending_proposals().len(), MAX_PENDING_PROPOSALS);

        reset_sweep_builds();
        let swept = s
            .bob_g
            .sweep_with_authorisation_only_filter(&s.bob)
            .expect("the blind sweep must still be buildable")
            .expect("something is swept");
        assert_eq!(
            swept.swept.len(),
            MAX_PENDING_PROPOSALS - 1,
            "every good add lands; only dave's is excluded",
        );
        assert_eq!(
            sweep_builds(),
            // the refused attempt, one probe per candidate, one retry for the
            // single rejected candidate, and the build that is finally staged.
            1 + MAX_PENDING_PROPOSALS + 1 + 1,
            "the search pays one probe per candidate, not one per pair",
        );
        assert!(
            !s.bob_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave.signature_public_key()),
            "and dave did not join on an unusable KeyPackage",
        );
    }

    /// The other end of the worst case: a pending set at the cap in which
    /// **nothing** is committable. The additive search pays one probe per
    /// candidate, then one retry probe per rejected candidate, so the bound is
    /// `2 * candidates` plus the failed attempt that sent us here — and nothing
    /// is ever staged, because nothing is accepted.
    ///
    /// Measured at the time of writing: **129 builds in ~8 ms** (debug; ~4 ms
    /// in release). Far cheaper per build than
    /// [`one_uncommittable_proposal_in_a_full_set_costs_only_itself`] despite
    /// twice the builds, because a probe here never carries more than the one
    /// candidate being offered.
    #[test]
    fn the_worst_case_build_count_is_two_probes_per_candidate() {
        let mut s = scene(0x57);
        for _ in 0..MAX_PENDING_PROPOSALS {
            s.bob_g
                .propose_add_member(&s.bob, kp(&s.charlie))
                .expect("bob re-adds charlie");
        }
        assert_eq!(s.bob_g.pending_proposals().len(), MAX_PENDING_PROPOSALS);

        reset_sweep_builds();
        assert!(
            s.bob_g
                .sweep_with_authorisation_only_filter(&s.bob)
                .expect("the blind sweep must still be buildable")
                .is_none(),
            "nothing in the set is committable",
        );
        assert_eq!(
            sweep_builds(),
            1 + 2 * MAX_PENDING_PROPOSALS,
            "one failed attempt, then one probe per candidate and one retry each",
        );
    }

    /// The direct-commit paths are **not** widened by any of this: they stay
    /// refused while anything is pending, including for the owner. The response
    /// the owner regains is the proposal one, which is the path a role-table
    /// group actually uses.
    #[test]
    fn the_direct_commit_paths_stay_refused_while_proposals_are_pending() {
        let mut s = scene(0x44);
        let bob_leaf = s.bob_g.own_leaf_index();
        flood_with_admin_adds(&mut s, 1);

        let dave = Identity::from_seed(&[0x60u8; 64]).expect("dave");
        for (op, err) in [
            (
                "add_member",
                s.alice_g
                    .add_member_from_wire(&s.alice, &kp_wire(&dave))
                    .err()
                    .map(|e| format!("{e}")),
            ),
            (
                "remove_member",
                s.alice_g
                    .remove_member(&s.alice, bob_leaf)
                    .err()
                    .map(|e| format!("{e}")),
            ),
            (
                "set_group_roles",
                s.alice_g
                    .set_group_roles(&s.alice, &RoleTable::new(pk32(&s.alice), vec![]))
                    .err()
                    .map(|e| format!("{e}")),
            ),
            (
                "self_update",
                s.alice_g
                    .self_update(&s.alice)
                    .err()
                    .map(|e| format!("{e}")),
            ),
        ] {
            let text = err.unwrap_or_else(|| panic!("{op} must still be refused"));
            assert!(
                text.contains("membership proposal(s) are pending"),
                "{op} must be refused by the pending-proposal guard, got {text:?}",
            );
        }
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
        // `Group::create` seeds the ADR-0022 role table with the creator as
        // owner, so this is a role-enabled group and (since ADR-0045 S7) the
        // approve is a proposal the owner then sweeps.
        let alice = Identity::from_seed(&[0xA0u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        let dave = Identity::from_seed(&[0xD0u8; 64]).expect("dave");
        let dave_pk = pk(&dave);

        let prop = external_join_proposal(&dave, &alice_g.group_id(), alice_g.epoch());
        let incoming = alice_g.process_incoming(&alice, &prop).expect("process");
        assert!(
            matches!(&incoming, Incoming::ExternalJoinProposal { joiner_signature_key, .. }
                if *joiner_signature_key == dave_pk),
            "got {incoming:?}",
        );
        assert_eq!(alice_g.pending_external_joins(), vec![dave_pk.clone()]);

        let epoch = alice_g.epoch();
        let proposal = alice_g
            .approve_external_join(&alice, &dave_pk)
            .expect("approve");
        assert!(
            !proposal.is_empty(),
            "the approve emits a proposal to publish"
        );
        assert_eq!(
            alice_g.epoch(),
            epoch,
            "an approve is a proposal: it must not advance the epoch",
        );
        assert!(
            !alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave_pk),
            "dave is not a member until somebody sweeps",
        );
        assert_eq!(alice_g.pending_proposals().len(), 1);

        // The owner sweeps its own approval into a commit.
        let sweep = alice_g
            .commit_pending_proposals(&alice)
            .expect("sweep")
            .expect("a commit was produced");
        assert!(sweep.welcome.is_some(), "the sweep carries dave's Welcome");
        assert!(
            alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == dave_pk),
            "dave should be a member after the sweep",
        );
        assert!(alice_g.pending_external_joins().is_empty());
    }

    /// The row is keyed on a NUL-terminated domain no OpenMLS label shares,
    /// and the row body's first byte is the version. Pinned so a drift fails
    /// here rather than as a request that quietly stops surviving restarts.
    #[test]
    fn pending_row_key_and_version_are_pinned() {
        assert_eq!(
            PENDING_EXTERNAL_JOINS_ROW_DOMAIN,
            b"lakko/pending-external-joins/v1\0"
        );
        let key = pending_external_joins_row_key(&[0xAB, 0xCD]);
        assert!(key.starts_with(PENDING_EXTERNAL_JOINS_ROW_DOMAIN));
        assert_eq!(
            &key[PENDING_EXTERNAL_JOINS_ROW_DOMAIN.len()..],
            &[0xAB, 0xCD]
        );
        assert_eq!(PENDING_EXTERNAL_JOINS_ROW_VERSION, 0x01);
        assert_eq!(
            encode_pending_external_joins(&[]).expect("encode"),
            vec![0x01, 0x00]
        );
    }

    /// A retained request writes the row; the last decision removes it, so a
    /// group with nothing pending leaves no trace in the snapshot.
    #[test]
    fn pending_row_is_written_on_retain_and_removed_when_empty() {
        let alice = Identity::from_seed(&[0xA7u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        let dave = Identity::from_seed(&[0xD7u8; 64]).expect("dave");
        let key = pending_external_joins_row_key(&alice_g.group_id());
        let has_row = |id: &Identity| {
            id.provider
                .storage()
                .values
                .read()
                .expect("lock")
                .contains_key(&key)
        };
        assert!(!has_row(&alice), "a fresh group has no row");

        let prop = external_join_proposal(&dave, &alice_g.group_id(), alice_g.epoch());
        alice_g.process_incoming(&alice, &prop).expect("process");
        assert!(has_row(&alice), "a retained request writes the row");

        alice_g
            .reject_external_join(&alice, &pk(&dave))
            .expect("reject");
        assert!(!has_row(&alice), "an empty set removes the row");
    }

    /// A row that does not decode is dropped and read as empty — local
    /// bookkeeping a joiner can refresh by asking again must never take the
    /// chat down with it.
    #[test]
    fn corrupt_pending_row_is_dropped_not_fatal() {
        let alice = Identity::from_seed(&[0xA8u8; 64]).expect("alice");
        let alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        let gid = alice_g.group_id();
        let key = pending_external_joins_row_key(&gid);
        for garbage in [
            Vec::new(),
            vec![0x02, 0x00],
            vec![0x01],
            vec![0x01, 0x01, 0x00, 0x00, 0x00, 0x05, 0xAA],
            vec![0x01, 0x01, 0xFF, 0xFF, 0xFF, 0xFF],
            vec![0x01, 0x00, 0x00],
        ] {
            alice
                .provider
                .storage()
                .values
                .write()
                .expect("lock")
                .insert(key.clone(), garbage.clone());
            assert!(
                decode_pending_external_joins(&garbage, &alice).is_err(),
                "{garbage:02x?} must not decode"
            );
            let loaded = Group::load(&alice, &gid)
                .expect("load never fails over the row")
                .expect("group present");
            assert!(loaded.pending_external_joins().is_empty());
            assert!(
                !alice
                    .provider
                    .storage()
                    .values
                    .read()
                    .expect("lock")
                    .contains_key(&key),
                "the corrupt row is deleted"
            );
        }
    }

    #[test]
    fn reject_removes_without_adding() {
        let alice = Identity::from_seed(&[0xA2u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        let dave = Identity::from_seed(&[0xD2u8; 64]).expect("dave");
        let dave_pk = pk(&dave);

        let prop = external_join_proposal(&dave, &alice_g.group_id(), alice_g.epoch());
        alice_g.process_incoming(&alice, &prop).expect("process");
        assert_eq!(alice_g.pending_external_joins().len(), 1);

        alice_g
            .reject_external_join(&alice, &dave_pk)
            .expect("reject");
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
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
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
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        assert!(alice_g.approve_external_join(&alice, b"nope").is_err());
        assert!(alice_g.reject_external_join(&alice, b"nope").is_err());
    }

    #[test]
    fn non_admin_approve_refused() {
        // alice = owner, bob = plain member, role table set.
        let alice = Identity::from_seed(&[0xA5u8; 64]).expect("alice");
        let bob = Identity::from_seed(&[0xB5u8; 64]).expect("bob");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");

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

        // Role-gated: a plain member is refused, and nothing is produced.
        let epoch = bob_g.epoch();
        assert!(
            bob_g.approve_external_join(&bob, &dave_pk).is_err(),
            "a non-admin must not approve an external join",
        );
        // dave still not a member.
        assert!(!bob_g
            .members()
            .iter()
            .any(|m| m.signature_public_key == dave_pk));
        // ADR-0045 S7 — the refusal must be a refusal, not a proposal peers
        // happen to drop. Asserting only "dave is not a member" would pass for
        // a bug that emitted an unauthorised Add and relied on every peer's
        // receipt gate to catch it: the local store would hold it, the pending
        // set would be occupied, and this device would offer it to its own
        // sweep.
        assert_eq!(
            bob_g.epoch(),
            epoch,
            "no epoch advance on a refused approve"
        );
        assert!(
            bob_g.pending_proposals().is_empty(),
            "a refused approve must publish no proposal and store none",
        );
        // And the request survives, so an admin can still act on it.
        assert_eq!(
            bob_g.pending_external_joins(),
            vec![dave_pk],
            "a refused approve must not consume the retained request",
        );
    }

    /// Full G-3 loop, in-process: a non-member crafts an external-join proposal
    /// via the public joiner API, an admin approves it, **a committer sweeps
    /// the approval into a commit** (ADR-0045 S7), the resulting Welcome rides
    /// a [`crate::WelcomeRendezvous`] envelope, and the joiner matches it by its
    /// own signature key + opens the inner Welcome — landing as a member at the
    /// post-sweep epoch. Proves the joiner side closes the loop end-to-end
    /// (ADR-0027 G-3).
    ///
    /// The sweep step is load-bearing rather than ceremonial: delete it and the
    /// Welcome the rendezvous carries does not exist, because an approve no
    /// longer produces one.
    #[test]
    fn joiner_api_full_loop_proposes_approves_joins_via_rendezvous() {
        let alice = Identity::from_seed(&[0xA9u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        let alice_pk = pk(&alice);
        let dave = Identity::from_seed(&[0xD9u8; 64]).expect("dave");
        let dave_pk = pk(&dave);

        // Joiner side: craft the proposal via the public API.
        let prop = dave
            .create_external_join_proposal(&alice_g.group_id(), alice_g.epoch())
            .expect("dave proposal");

        // Admin side: surface + approve. The approve is a PROPOSAL now.
        let incoming = alice_g.process_incoming(&alice, &prop).expect("process");
        assert!(
            matches!(&incoming, Incoming::ExternalJoinProposal { joiner_signature_key, .. }
                if *joiner_signature_key == dave_pk),
            "got {incoming:?}",
        );
        let epoch_at_approve = alice_g.epoch();
        let approval = alice_g
            .approve_external_join(&alice, &dave_pk)
            .expect("approve");
        assert!(!approval.is_empty());
        assert_eq!(
            alice_g.epoch(),
            epoch_at_approve,
            "the approve advances nothing",
        );

        // Committer side: the acting committer sweeps the approval into one
        // commit, and THAT is what produces the Welcome.
        let sweep = alice_g
            .commit_pending_proposals(&alice)
            .expect("sweep")
            .expect("a commit was produced");
        assert_eq!(sweep.swept.len(), 1, "the sweep carried the approval");
        assert!(
            matches!(&sweep.swept[0].kind, ProposalKind::Add { signature_key, .. } if *signature_key == dave_pk),
            "and it is dave's Add: {:?}",
            sweep.swept[0].kind,
        );
        let welcome = sweep
            .welcome
            .expect("a sweep carrying an Add has a Welcome");

        // Transport: wrap the Welcome in a rendezvous envelope (what the
        // sweeping device posts to the group journal).
        let envelope = crate::WelcomeRendezvous {
            joiner_signature_key: dave_pk.clone(),
            welcome: encode_message(&welcome).expect("encode welcome"),
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

    // ========================================================================
    // ADR-0045 S7 — the external-join approve on the proposal path
    // ========================================================================

    /// **The half of the old `sweep_never_includes_an_external_join_add` that
    /// the store-emptiness assertion can no longer carry.**
    ///
    /// Under S7 the pending store legitimately holds the admin's Add, so
    /// "nothing is pending" no longer says anything about the stranger. The
    /// property that must survive is narrower and sharper: a `NewMemberProposal`
    /// sender is excluded by the sweep filter **even when it is already sitting
    /// in the store**, which is the state the shipped receive path structurally
    /// never reaches — so it is planted here through the `#[cfg(test)]`
    /// adversary seam.
    ///
    /// The exclusion is not a dedicated arm. It falls out of where authorship
    /// is resolved: an external sender maps to no member leaf, so
    /// [`crate::group_proposals::authorise`] has no `Proposer` and refuses every
    /// kind. That is deliberately the only copy of the rule — a second,
    /// external-join-shaped arm beside it would be a place for the two to
    /// disagree — so it is measured here rather than duplicated.
    #[test]
    fn a_planted_external_join_add_is_still_excluded_by_the_sweep() {
        let alice = Identity::from_seed(&[0xAAu8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        let stranger = Identity::from_seed(&[0xEAu8; 64]).expect("stranger");
        let stranger_pk = pk(&stranger);

        let external = external_join_proposal(&stranger, &alice_g.group_id(), alice_g.epoch());

        // Plant it where the shipped path never puts it.
        alice_g
            .ungated_store_external_join(&alice, &external)
            .expect("adversary plants the stranger's Add in the MLS store");

        // The plant landed — otherwise this test would pass vacuously.
        let pending = alice_g.pending_proposals();
        assert_eq!(pending.len(), 1, "the adversary's plant is in the store");
        assert_eq!(
            pending[0].sender_leaf, None,
            "and its sender resolves to no member leaf, which is the whole point",
        );
        assert!(matches!(pending[0].kind, ProposalKind::Add { .. }));

        // The owner sweeps. Nothing is committed and nobody is admitted.
        assert!(
            alice_g
                .commit_pending_proposals(&alice)
                .expect("sweep")
                .is_none(),
            "a sweep must not admit a stranger it was handed directly",
        );
        assert!(
            !alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == stranger_pk),
            "the stranger must not be a member",
        );
    }

    /// **Control for the test above.** With the sweep filter replaced by
    /// OpenMLS's own `|_| true` — which is exactly what
    /// `commit_to_pending_proposals` hardcodes, and what ADR-0045 rejects — the
    /// very same planted Add **does** admit the stranger.
    ///
    /// Without this the exclusion could be passing for the wrong reason (the
    /// plant failing to land, openmls refusing to commit an external proposal
    /// at all), and the assertion "the stranger is not a member" would be
    /// unfalsifiable.
    #[test]
    fn control_an_unfiltered_sweep_admits_the_planted_stranger() {
        let alice = Identity::from_seed(&[0xA6u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        let stranger = Identity::from_seed(&[0xE6u8; 64]).expect("stranger");
        let stranger_pk = pk(&stranger);

        let external = external_join_proposal(&stranger, &alice_g.group_id(), alice_g.epoch());
        alice_g
            .ungated_store_external_join(&alice, &external)
            .expect("plant");

        alice_g
            .ungated_sweep(&alice)
            .expect("an unfiltered sweep commits everything it is offered");
        assert!(
            alice_g
                .members()
                .iter()
                .any(|m| m.signature_public_key == stranger_pk),
            "the control must reproduce the hazard: an unfiltered sweep lets the stranger in",
        );
    }

    /// ADR-0045 S7 — a stranger's own external-join Add never reaches the MLS
    /// proposal store at all, whichever receive path processes it. This is the
    /// load-bearing layer the exclusion above is only the second line of.
    #[test]
    fn a_strangers_own_add_never_enters_the_proposal_store() {
        let stranger = Identity::from_seed(&[0xEBu8; 64]).expect("stranger");
        for (tag, gated) in [(0xABu8, false), (0xACu8, true)] {
            let alice = Identity::from_seed(&[tag; 64]).expect("alice");
            let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
            let external = external_join_proposal(&stranger, &alice_g.group_id(), alice_g.epoch());
            let incoming = if gated {
                alice_g.process_incoming_verifying_roles(&alice, &external)
            } else {
                alice_g.process_incoming(&alice, &external)
            }
            .expect("process the stranger's request");
            assert!(matches!(incoming, Incoming::ExternalJoinProposal { .. }));
            assert_eq!(
                alice_g.pending_external_joins().len(),
                1,
                "it is quarantined for an explicit decision (gated={gated})",
            );
            assert!(
                alice_g.pending_proposals().is_empty(),
                "and never in the swept store (gated={gated})",
            );
        }
    }

    /// ADR-0045 S7 — a group with no role table has no committer, so a proposal
    /// published into it would never be swept. `Group::approve_external_join`
    /// refuses it rather than quietly direct-committing; the direct route lives
    /// on `PublisherGroup`, and its **return type** is what tells the two
    /// apart.
    #[test]
    fn a_table_less_group_refuses_the_proposal_approve_and_keeps_the_direct_one() {
        use crate::publisher_group::{ChannelId, PublisherGroup, CHANNEL_ID_LEN};

        let alice = Identity::from_seed(&[0xADu8; 64]).expect("alice");
        let cid = ChannelId::try_from_slice(&[0x77u8; CHANNEL_ID_LEN]).expect("channel id");
        let mut pg = PublisherGroup::create(&alice, &cid).expect("publisher group");
        assert!(
            pg.inner().group_roles().is_none(),
            "a PublisherGroup carries no role table, which is why it keeps direct commits",
        );

        let bob = Identity::from_seed(&[0xBDu8; 64]).expect("bob");
        let bob_pk = pk(&bob);
        let req = external_join_proposal(&bob, &pg.group_id(), pg.epoch());
        pg.inner_mut()
            .process_incoming(&alice, &req)
            .expect("process bob's request");

        // The proposal path is refused, by name.
        let refusal = pg
            .inner_mut()
            .approve_external_join(&alice, &bob_pk)
            .map(|_| ())
            .expect_err("a table-less group has no committer");
        assert!(
            refusal.to_string().contains("no role table"),
            "the refusal must say why: {refusal}",
        );
        assert!(pg.inner().pending_proposals().is_empty());

        // And the group's SHAPE is diagnosed ahead of the request lookup: a
        // caller on the wrong entry point must be told which entry point is
        // wrong, not that its perfectly valid ref is unknown. This is what the
        // explicit `role_table_or_refuse` at the top of the proposal approve
        // buys — `propose_add_member` would refuse a table-less group too, but
        // only after the lookup had already failed for an unrelated reason.
        let misdirected = pg
            .inner_mut()
            .approve_external_join(&alice, b"not a pending request")
            .map(|_| ())
            .expect_err("still refused");
        assert!(
            misdirected.to_string().contains("no role table"),
            "the group's shape must be diagnosed before the request lookup: {misdirected}",
        );

        // The direct path works, and hands back a Commit + a Welcome.
        let addition = pg
            .approve_external_join(&alice, &bob_pk)
            .expect("the direct approve is the one that survives here");
        let welcome = encode_message(&addition.welcome).expect("encode welcome");
        Group::join_from_welcome(&welcome, &bob).expect("bob joins from the Welcome");
        assert!(pg.inner().pending_external_joins().is_empty());
    }

    /// ADR-0045 S7 — and the direct approve refuses a group that *does* carry a
    /// role table, so it can never become a second ungated route into the
    /// membership of a group that has a committer (fail-closed, invariant #4).
    #[test]
    fn the_direct_approve_refuses_a_group_that_has_a_committer() {
        let alice = Identity::from_seed(&[0xAEu8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        let dave = Identity::from_seed(&[0xDEu8; 64]).expect("dave");
        let dave_pk = pk(&dave);
        let req = external_join_proposal(&dave, &alice_g.group_id(), alice_g.epoch());
        alice_g.process_incoming(&alice, &req).expect("process");

        let epoch = alice_g.epoch();
        let refusal = alice_g
            .approve_external_join_directly(&alice, &dave_pk)
            .map(|_| ())
            .expect_err("the direct approve must refuse a role-enabled group");
        assert!(
            refusal.to_string().contains("role table"),
            "the refusal must say why: {refusal}",
        );
        assert_eq!(alice_g.epoch(), epoch, "and touch nothing");
        assert_eq!(
            alice_g.pending_external_joins(),
            vec![dave_pk],
            "and leave the request where it was",
        );
    }

    /// ADR-0045 S7 — a pending membership proposal no longer blocks an approve.
    ///
    /// It used to: the direct approve was guarded by
    /// `refuse_if_proposals_pending`, and under ADR-0045 a pending proposal is
    /// the *normal* state, so an admin deferring to a live owner could
    /// essentially never approve. The two are now the same kind of thing and
    /// compose into one sweep.
    #[test]
    fn an_approve_is_no_longer_blocked_by_a_pending_membership_proposal() {
        let alice = Identity::from_seed(&[0xAFu8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
        let bob = Identity::from_seed(&[0xBFu8; 64]).expect("bob");
        let dave = Identity::from_seed(&[0xDFu8; 64]).expect("dave");
        let dave_pk = pk(&dave);

        // Something ordinary is already pending.
        alice_g
            .propose_add_member_from_wire(&alice, &kp_wire(&bob))
            .expect("alice proposes adding bob");
        assert_eq!(alice_g.pending_proposals().len(), 1);

        // A stranger asks, and the owner approves anyway.
        let req = external_join_proposal(&dave, &alice_g.group_id(), alice_g.epoch());
        alice_g.process_incoming(&alice, &req).expect("process");
        alice_g
            .approve_external_join(&alice, &dave_pk)
            .expect("an approve must not be blocked by a pending proposal");
        assert_eq!(alice_g.pending_proposals().len(), 2);

        // Both land in one commit.
        let sweep = alice_g
            .commit_pending_proposals(&alice)
            .expect("sweep")
            .expect("a commit was produced");
        assert_eq!(sweep.swept.len(), 2, "the add and the approval compose");
        for id in [&bob, &dave] {
            assert!(
                alice_g
                    .members()
                    .iter()
                    .any(|m| m.signature_public_key == pk(id)),
                "both joined",
            );
        }
    }

    /// ADR-0028 §3 — a non-member can read a group's current epoch from the
    /// public MLS framing of a journal message (no group secret), which is what
    /// lets the join-ticket joiner craft an external-join proposal for the right
    /// epoch.
    #[test]
    fn epoch_of_message_reads_public_framing() {
        let alice = Identity::from_seed(&[0xE0u8; 64]).expect("alice");
        let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create"); // epoch 0
        let bob = Identity::from_seed(&[0xE1u8; 64]).expect("bob");

        // An application message at epoch 0 reports epoch 0 from its framing.
        let m0 = wire(
            alice_g
                .create_application_message(&alice, b"hello")
                .expect("app msg"),
        );
        assert_eq!(crate::epoch_of_message(&m0), Some(0));
        assert_eq!(crate::epoch_after_message(&m0), Some(0));

        // Add bob → alice merges → epoch advances. The Welcome has no epoch.
        let add = alice_g
            .add_member_from_wire(&alice, &kp_wire(&bob))
            .expect("add bob");
        let welcome = wire(add.welcome);
        assert_eq!(crate::epoch_of_message(&welcome), None);
        assert_eq!(crate::epoch_after_message(&welcome), None);
        // The Commit itself is framed at the epoch it was created in (0), but
        // the group it leaves behind is at 1: a joiner reading a journal whose
        // head is this Commit must target 1, not 0.
        let commit = wire(add.commit);
        assert_eq!(crate::epoch_of_message(&commit), Some(0));
        assert_eq!(crate::epoch_after_message(&commit), Some(1));

        // A fresh app message now reports the advanced epoch.
        let m1 = wire(
            alice_g
                .create_application_message(&alice, b"world")
                .expect("app msg 2"),
        );
        assert_eq!(alice_g.epoch(), 1);
        assert_eq!(crate::epoch_of_message(&m1), Some(alice_g.epoch()));
        assert_eq!(crate::epoch_after_message(&m1), Some(alice_g.epoch()));

        // Non-MLS / empty bytes → None (the caller treats this as epoch 0).
        assert_eq!(crate::epoch_of_message(&[0xFFu8; 12]), None);
        assert_eq!(crate::epoch_of_message(&[]), None);
    }
}

/// ADR-0060 — the peer-side halves of the one-to-one rule and the kind's
/// immutability, driven by a committer that skips every local check (the
/// modified-client adversary of `proposal_gate_tests`), plus the
/// `join_from_welcome` classification check.
#[cfg(test)]
mod one_to_one_gate_tests {
    use super::*;
    use crate::key_package;

    fn wire(m: MlsMessageOut) -> Vec<u8> {
        encode_message(&m).expect("encode")
    }

    fn kp_wire(id: &Identity) -> Vec<u8> {
        key_package::serialize(&id.create_key_package().expect("kp")).expect("serialize kp")
    }

    fn kp(id: &Identity) -> KeyPackage {
        key_package::deserialize(&kp_wire(id))
            .expect("deserialize")
            .validate(id.provider.crypto(), ProtocolVersion::Mls10)
            .expect("validate")
    }

    fn device(owner: &Identity, tag: u8) -> Identity {
        Identity::from_seed_bound(&[tag; 32], &owner.ed25519_seed_bytes(), 1_700_000_000)
            .expect("bound device")
    }

    fn reason(e: &Error) -> Option<RefusalReason> {
        e.refusal_reason()
    }

    impl Group {
        /// **Adversary.** A direct add that skips `add_members`' local checks.
        fn ungated_add_member(&mut self, identity: &Identity, kp: KeyPackage) -> MlsMessageOut {
            let (commit, _w, _gi) = self
                .inner
                .add_members(&identity.provider, &identity.signature_keys, &[kp])
                .expect("ungated add");
            self.inner
                .merge_pending_commit(&identity.provider)
                .expect("merge");
            commit
        }

        /// **Adversary.** A GroupContext rewrite that skips every local check.
        fn ungated_set_extensions(
            &mut self,
            identity: &Identity,
            exts: Extensions<GroupContext>,
        ) -> MlsMessageOut {
            let (commit, _w, _gi) = self
                .inner
                .update_group_context_extensions(&identity.provider, exts, &identity.signature_keys)
                .expect("ungated rewrite");
            self.inner
                .merge_pending_commit(&identity.provider)
                .expect("merge");
            commit
        }
    }

    /// a1 founds a one-to-one with bob (b1) and alice's second device a2.
    struct Scene {
        a1: Identity,
        a2: Identity,
        b1: Identity,
        a1_g: Group,
        a2_g: Group,
        b1_g: Group,
    }

    fn scene(tag: u8) -> Scene {
        let a = Identity::from_seed(&[0x10 ^ tag; 64]).expect("a");
        let a1 = device(&a, 0x11 ^ tag);
        let a2 = device(&a, 0x12 ^ tag);
        let b = Identity::from_seed(&[0x20 ^ tag; 64]).expect("b");
        let b1 = device(&b, 0x21 ^ tag);
        let mut a1_g = Group::create(&a1, GroupKind::OneToOne).expect("found");
        let add = a1_g
            .add_members_from_wire(&a1, &[&kp_wire(&b1), &kp_wire(&a2)])
            .expect("founding commit");
        let welcome = wire(add.welcome);
        let b1_g = Group::join_from_welcome(&welcome, &b1).expect("b1 joins");
        let a2_g = Group::join_from_welcome(&welcome, &a2).expect("a2 joins");
        Scene {
            a1,
            a2,
            b1,
            a1_g,
            a2_g,
            b1_g,
        }
    }

    #[test]
    fn the_receipt_gate_drops_a_third_persons_add_in_a_one_to_one() {
        let mut s = scene(0x01);
        let c = Identity::from_seed(&[0x31; 64]).expect("c");
        let hostile = s
            .a1_g
            .ungated_propose_add(&s.a1, kp(&c))
            .expect("the owner proposes without its local check");
        let Incoming::Proposal(record) = s
            .b1_g
            .process_incoming_verifying_roles(&s.b1, &hostile)
            .expect("b1 processes it")
        else {
            panic!("expected a proposal");
        };
        assert!(!record.stored, "a third person's add is never stored");
        assert!(s.b1_g.pending_proposals().is_empty());
    }

    #[test]
    fn the_sweep_never_carries_a_third_persons_add() {
        let mut s = scene(0x02);
        let c = Identity::from_seed(&[0x32; 64]).expect("c");
        let hostile = s
            .a1_g
            .ungated_propose_add(&s.a1, kp(&c))
            .expect("proposed ungated");
        s.a2_g
            .ungated_store(&s.a2, &hostile)
            .expect("a2 stores it ungated");
        // a2 is the owner account's device; its sweep must emit nothing.
        let swept = s.a2_g.commit_pending_proposals(&s.a2).expect("sweep");
        assert!(swept.is_none(), "the commit-wide rule excludes it");
        assert_eq!(s.a2_g.members().len(), 3);
    }

    #[test]
    fn the_peer_commit_gate_refuses_a_third_person_and_does_not_merge() {
        let mut s = scene(0x03);
        let c = Identity::from_seed(&[0x33; 64]).expect("c");
        let commit = s.a1_g.ungated_add_member(&s.a1, kp(&c));
        let epoch = s.b1_g.epoch();
        let err = s
            .b1_g
            .process_incoming_verifying_roles(&s.b1, &wire(commit))
            .expect_err("the peer refuses");
        assert_eq!(reason(&err), Some(RefusalReason::OneToOneMembershipFixed));
        assert_eq!(s.b1_g.epoch(), epoch, "a refused commit is not merged");
        assert_eq!(s.b1_g.members().len(), 3);
    }

    #[test]
    fn the_peer_commit_gate_refuses_removing_a_person() {
        let mut s = scene(0x04);
        let b1_leaf = s
            .a1_g
            .members()
            .iter()
            .find(|m| m.signature_public_key == s.b1.signature_public_key())
            .expect("b1")
            .leaf_index;
        let commit = s
            .a1_g
            .ungated_remove_member(&s.a1, LeafNodeIndex::new(b1_leaf))
            .expect("ungated remove");
        let err = s
            .a2_g
            .process_incoming_verifying_roles(&s.a2, &wire(commit))
            .expect_err("the peer refuses");
        assert_eq!(reason(&err), Some(RefusalReason::OneToOneMembershipFixed));
        assert_eq!(s.a2_g.members().len(), 3);
    }

    #[test]
    fn the_peer_commit_gate_refuses_any_rewrite_in_a_one_to_one() {
        let mut s = scene(0x05);
        // Even a rewrite that keeps the kind byte-identical: a one-to-one's
        // GroupContext is frozen from creation.
        let exts = s
            .a1_g
            .role_rewrite_extensions(&s.a1_g.group_roles().expect("table"))
            .expect("exts");
        let commit = s.a1_g.ungated_set_extensions(&s.a1, exts);
        let err = s
            .b1_g
            .process_incoming_verifying_roles(&s.b1, &wire(commit))
            .expect_err("the peer refuses");
        assert_eq!(reason(&err), Some(RefusalReason::OneToOneHasNoRoles));
    }

    fn group_pair(tag: u8) -> (Identity, Identity, Group, Group) {
        let a = Identity::from_seed(&[0x40 ^ tag; 64]).expect("a");
        let b = Identity::from_seed(&[0x50 ^ tag; 64]).expect("b");
        let mut a_g = Group::create(&a, GroupKind::Group).expect("group");
        let add = a_g.add_member_from_wire(&a, &kp_wire(&b)).expect("add");
        let b_g = Group::join_from_welcome(&wire(add.welcome), &b).expect("join");
        (a, b, a_g, b_g)
    }

    /// The owner's rewrite, with the `0xFF03` extension transformed by `f`.
    fn rewrite_with_kind(
        g: &Group,
        f: impl Fn(Extension) -> Option<Extension>,
    ) -> Extensions<GroupContext> {
        let current = g
            .role_rewrite_extensions(&g.group_roles().expect("table"))
            .expect("exts");
        let list: Vec<Extension> = current
            .iter()
            .cloned()
            .filter_map(|e| match &e {
                Extension::Unknown(t, _) if *t == GROUP_KIND_EXTENSION_TYPE => f(e),
                _ => Some(e),
            })
            .collect();
        Extensions::from_vec(list).expect("valid list")
    }

    #[test]
    fn a_commit_that_changes_the_kind_is_refused_on_the_peer() {
        let (a, b, mut a_g, mut b_g) = group_pair(0x06);
        let exts = rewrite_with_kind(&a_g, |_| Some(GroupKind::OneToOne.extension()));
        // The honest local path refuses it too.
        let local = a_g
            .set_group_context_extensions(&a, exts.clone())
            .expect_err("local refusal");
        assert_eq!(reason(&local), Some(RefusalReason::GroupKindChanged));
        let commit = a_g.ungated_set_extensions(&a, exts);
        let err = b_g
            .process_incoming_verifying_roles(&b, &wire(commit))
            .expect_err("the peer refuses");
        assert_eq!(reason(&err), Some(RefusalReason::GroupKindChanged));
        assert_eq!(b_g.group_kind(), Some(GroupKind::Group));
    }

    #[test]
    fn a_commit_that_drops_the_kind_is_refused_on_the_peer() {
        let (a, b, mut a_g, mut b_g) = group_pair(0x07);
        let exts = rewrite_with_kind(&a_g, |_| None);
        let commit = a_g.ungated_set_extensions(&a, exts);
        let err = b_g
            .process_incoming_verifying_roles(&b, &wire(commit))
            .expect_err("the peer refuses");
        assert_eq!(reason(&err), Some(RefusalReason::GroupKindChanged));
        assert_eq!(b_g.group_kind(), Some(GroupKind::Group));
    }

    #[test]
    fn the_receipt_gate_drops_a_proposal_that_changes_the_kind() {
        let (a, b, a_g, mut b_g) = group_pair(0x08);
        let exts = rewrite_with_kind(&a_g, |_| Some(GroupKind::OneToOne.extension()));
        let mut a_g = a_g;
        let (proposal, _r) = a_g
            .inner
            .propose_group_context_extensions(&a.provider, exts, &a.signature_keys)
            .expect("ungated proposal");
        let Incoming::Proposal(record) = b_g
            .process_incoming_verifying_roles(&b, &wire(proposal))
            .expect("processed")
        else {
            panic!("expected a proposal");
        };
        assert!(!record.stored, "a kind-changing rewrite is never stored");
    }

    /// Found a conversation-shaped group whose GroupContext carries the role
    /// table and whatever `kind` says, add `joiner`, and return the Welcome.
    fn welcome_into(kind: Option<Extension>, tag: u8) -> (Vec<u8>, Identity) {
        let founder = Identity::from_seed(&[0x60 ^ tag; 64]).expect("founder");
        let joiner = Identity::from_seed(&[0x70 ^ tag; 64]).expect("joiner");
        let owner = founder.account_public_key().expect("key");
        let mut exts = vec![
            role_required_capabilities(),
            Extension::Unknown(
                ROLE_EXTENSION_TYPE,
                UnknownExtension(RoleTable::owner_only(owner).encode()),
            ),
        ];
        exts.extend(kind);
        let cfg = MlsGroupCreateConfig::builder()
            .ciphersuite(founder.ciphersuite())
            .use_ratchet_tree_extension(true)
            .capabilities(crate::device_group::lakko_leaf_capabilities(
                founder.ciphersuite(),
            ))
            .with_group_context_extensions(Extensions::from_vec(exts).expect("exts"))
            .build();
        let mut g =
            Group::create_inner(&founder, &cfg, &[tag; GROUP_ID_LEN], "test").expect("founded");
        let add = g
            .add_member_from_wire(&founder, &kp_wire(&joiner))
            .expect("add");
        (wire(add.welcome), joiner)
    }

    #[test]
    fn a_conversation_welcome_without_a_kind_is_not_joined() {
        let (welcome, joiner) = welcome_into(None, 0x09);
        assert!(Group::join_from_welcome(&welcome, &joiner).is_err());
    }

    #[test]
    fn a_conversation_welcome_with_an_unreadable_kind_is_not_joined() {
        let bad = Extension::Unknown(
            GROUP_KIND_EXTENSION_TYPE,
            UnknownExtension(vec![0x01, 0x09]),
        );
        let (welcome, joiner) = welcome_into(Some(bad), 0x0A);
        assert!(Group::join_from_welcome(&welcome, &joiner).is_err());
    }

    #[test]
    fn control_a_welcome_with_a_readable_kind_is_joined() {
        let (welcome, joiner) = welcome_into(Some(GroupKind::OneToOne.extension()), 0x0B);
        let g = Group::join_from_welcome(&welcome, &joiner).expect("joins");
        assert_eq!(g.group_kind(), Some(GroupKind::OneToOne));
    }
}
