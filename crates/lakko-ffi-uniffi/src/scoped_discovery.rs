// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of ADR-0060 "who can find me" — scoped discovery.
//!
//! Mirror of `lakko-ffi-wasm/src/scoped_discovery.rs` over the same cores:
//!
//! - **Reach keys** (`lakko_contacts::reach`): [`reach_seed`],
//!   [`reach_verifying_key`], [`derive_reach_grant`], [`encode_reach_grant`] /
//!   [`decode_reach_grant`] and the constants. A reach key is what a closed
//!   identity mailbox accepts a write under; the seed is SECRET — whoever
//!   holds it can write to the owner's mailbox — so it only ever travels
//!   inside an E2EE carrier (a profile grant, an introduction payload, a 1:1
//!   message).
//! - **Contacts of contacts** (`lakko_contacts::introduction`):
//!   [`compose_introduction`], [`introduction_tags`], [`fof_tag`],
//!   [`open_introduction`], the offer and bundle envelopes,
//!   [`bundle_is_newer`]. Records cross this boundary as their wire bytes;
//!   the search terms as their 64-byte server OPRF outputs, computed with the
//!   `OprfBlinder` name / username constructors (`discovery.rs`).
//! - **The setting and the reach state** (`lakko_contacts::visibility`):
//!   [`FfiVisibilitySetting`], [`default_visibility_setting`], the two
//!   envelopes, the orders, [`allows_scope`], [`allows_contacts_of_contacts`],
//!   [`reach_state_version_of`]. The Settings transitions that edit a setting
//!   live in `ui_rules.rs`.
//! - **Username certificate** (`lakko_attest::username_cert`):
//!   [`verify_username_binding`], verify only — signing is discovery's.
//! - **Committer delegations** (§6, `lakko_contacts::committer_delegation` +
//!   `lakko_ds_proof::committer`): [`compose_committer_delegation_envelope`],
//!   [`decode_committer_delegation_envelope`], [`check_committer_delegations`],
//!   [`committer_delegation_for`], [`decode_delegation`],
//!   [`delegation_header_value`]; the blobs are signed with
//!   `sign_delegation_blob` (`ds_proof.rs`).
//!
//! Every order is strictly-greater (a re-send carries its original stamp and
//! is not an update, the ADR-0044 lesson); persist a stamp and re-emit it
//! verbatim.

use core::fmt;

use lakko_attest::username_cert::{
    verify_username_binding as rust_verify_username_binding, UsernameCertError,
    UsernameCertificate as RustUsernameCertificate, OPRF_HASH_LEN,
};
use lakko_contacts::committer_delegation::{
    compose_committer_delegation as rust_compose_committer_delegation,
    decode_committer_delegation as rust_decode_committer_delegation, CommitterDelegationEnvelope,
    CommitterDelegationError, ADDED_ACCOUNT_KEY_LEN, COMMITTER_DELEGATION_BLOB_LEN,
    MAX_COMMITTER_DELEGATIONS,
};
use lakko_contacts::introduction::{
    bundle_is_newer as rust_bundle_is_newer, compose_bundle as rust_compose_bundle,
    compose_introduction as rust_compose_introduction,
    compose_introduction_offer as rust_compose_introduction_offer,
    decode_introduction_offer as rust_decode_introduction_offer,
    decode_introductions as rust_decode_introductions, fof_tag as rust_fof_tag,
    introduction_tags as rust_introduction_tags, open_introduction as rust_open_introduction,
    IntroductionError, IntroductionPayload as RustIntroductionPayload,
    IntroductionRecord as RustIntroductionRecord, IntroductionsBundle as RustIntroductionsBundle,
    BUNDLE_SIZE_BUCKETS, FOF_TAG_LEN, IDENTITY_PK_LEN, MAX_BUNDLE_RECORDS, MAX_INTRODUCTION_TERMS,
    OPRF_OUTPUT_LEN,
};
use lakko_contacts::profile::ACCOUNT_ID_LEN;
use lakko_contacts::reach::{
    reach_seed as rust_reach_seed, reach_verifying_key as rust_reach_verifying_key, ReachClass,
    ReachError, ReachGrant as RustReachGrant, MAX_REACH_SCOPE_ID_LEN, REACH_GRANT_LEN,
    REACH_SEED_LEN,
};
use lakko_contacts::visibility::{
    compose_reach_state as rust_compose_reach_state, compose_visibility as rust_compose_visibility,
    decode_reach_state as rust_decode_reach_state, decode_visibility as rust_decode_visibility,
    reach_state_is_newer as rust_reach_state_is_newer,
    visibility_is_newer as rust_visibility_is_newer, GroupsForm, ReachState as RustReachState,
    ReachStateEntry as RustReachStateEntry, ScopeId as RustScopeId, ScopeKind, VisibilityError,
    VisibilityMode, VisibilitySetting as RustVisibilitySetting, MAX_CHOSEN_SCOPES,
    MAX_REACH_STATE_ENTRIES, MAX_SCOPE_ID_LEN,
};
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::VerifyingKey;
use lakko_ds_proof::{
    check_committer_delegations as rust_check_committer_delegations,
    delegation_for as rust_delegation_for, CommitterDelegationCheckError, Delegation,
    DelegationError,
};

use crate::FfiError;

// ============================================================================
// Error mapping — `<code>: <reason>`
// ============================================================================

impl From<ReachError> for FfiError {
    fn from(e: ReachError) -> Self {
        Self::Reach(format!("{}: {e}", e.code()))
    }
}

impl From<IntroductionError> for FfiError {
    fn from(e: IntroductionError) -> Self {
        match e {
            // The payload's name and username follow the profile rule, so
            // they surface as the same typed variants a profile's do.
            IntroductionError::DisplayName(d) => d.into(),
            IntroductionError::Username(u) => u.into(),
            other => Self::Introduction(format!("{}: {other}", other.code())),
        }
    }
}

impl From<VisibilityError> for FfiError {
    fn from(e: VisibilityError) -> Self {
        Self::Visibility(format!("{}: {e}", e.code()))
    }
}

/// The frozen code of a username-certificate refusal. `lakko-attest` has no
/// `code()` of its own; `lakko-ffi-wasm` carries the same table (pinned by a
/// test on each side).
#[must_use]
pub(crate) const fn username_cert_code(e: &UsernameCertError) -> &'static str {
    match e {
        UsernameCertError::BadLength { .. } => "badLength",
        UsernameCertError::BadVersion(_) => "badVersion",
        UsernameCertError::BadSignature => "badSignature",
        UsernameCertError::UsernameMismatch => "usernameMismatch",
        UsernameCertError::AccountMismatch => "accountMismatch",
    }
}

impl From<UsernameCertError> for FfiError {
    fn from(e: UsernameCertError) -> Self {
        Self::UsernameCertificate(format!("{}: {e}", username_cert_code(&e)))
    }
}

fn reach_arg(code: &str, what: &str, want: usize, got: usize) -> FfiError {
    FfiError::Reach(format!("{code}: {what} must be {want} bytes, got {got}"))
}

fn seed32(bytes: &[u8], what: &str) -> Result<[u8; REACH_SEED_LEN], FfiError> {
    <[u8; REACH_SEED_LEN]>::try_from(bytes)
        .map_err(|_| reach_arg("seedLength", what, REACH_SEED_LEN, bytes.len()))
}

// ============================================================================
// Reach keys — ADR-0060 §1
// ============================================================================

/// Who a reach key is handed to (ADR-0060 §1). Mirror of
/// [`lakko_contacts::reach::ReachClass`]; the wire bytes live in the core.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiReachClass {
    /// Every contact. Empty scope.
    Contact,
    /// Whoever holds one invite link. Scope: the capability id.
    Link,
    /// The one party a request of the owner's went to. Scope: a random id.
    Reply,
    /// Members of a group the owner lets find them. Scope: the group id.
    Group,
    /// Every co-member of a group, whatever the setting. Scope: the group id.
    Member,
    /// Members of a channel the owner lets find them. Scope: the channel id.
    Channel,
    /// Contacts of contacts. Empty scope.
    Fof,
}

impl From<FfiReachClass> for ReachClass {
    fn from(c: FfiReachClass) -> Self {
        match c {
            FfiReachClass::Contact => Self::Contact,
            FfiReachClass::Link => Self::Link,
            FfiReachClass::Reply => Self::Reply,
            FfiReachClass::Group => Self::Group,
            FfiReachClass::Member => Self::Member,
            FfiReachClass::Channel => Self::Channel,
            FfiReachClass::Fof => Self::Fof,
        }
    }
}

impl From<ReachClass> for FfiReachClass {
    fn from(c: ReachClass) -> Self {
        match c {
            ReachClass::Contact => Self::Contact,
            ReachClass::Link => Self::Link,
            ReachClass::Reply => Self::Reply,
            ReachClass::Group => Self::Group,
            ReachClass::Member => Self::Member,
            ReachClass::Channel => Self::Channel,
            ReachClass::Fof => Self::Fof,
        }
    }
}

/// A reach key handed to someone (ADR-0060 §1): class, version, seed. Mirror
/// of [`lakko_contacts::reach::ReachGrant`].
///
/// **`seed` is secret**: whoever holds it can write to the owner's mailbox.
/// Keep it only in encrypted storage and never log it — `Debug` here prints
/// `<redacted>`.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiReachGrant {
    /// Who the key is for.
    pub class: FfiReachClass,
    /// The key version, bumped on every rotation.
    pub version: u32,
    /// The 32-byte Ed25519 seed.
    pub seed: Vec<u8>,
}

impl fmt::Debug for FfiReachGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FfiReachGrant")
            .field("class", &self.class)
            .field("version", &self.version)
            .field("seed", &"<redacted>")
            .finish()
    }
}

impl From<RustReachGrant> for FfiReachGrant {
    fn from(g: RustReachGrant) -> Self {
        Self {
            class: g.class.into(),
            version: g.version,
            seed: g.seed.as_bytes().to_vec(),
        }
    }
}

impl TryFrom<FfiReachGrant> for RustReachGrant {
    type Error = FfiError;

    fn try_from(g: FfiReachGrant) -> Result<Self, FfiError> {
        Ok(Self {
            class: g.class.into(),
            version: g.version,
            seed: SecretBytes::new(seed32(&g.seed, "reach grant seed")?),
        })
    }
}

/// Derive the 32-byte reach seed for `(class, scope_id, version)` from the
/// 32-byte account seed (the Ed25519 seed of the account identity). Every
/// device of the account derives the same seed, so nothing but the version
/// counters (the reach state) crosses between them.
///
/// # Errors
///
/// * [`FfiError::Reach`] `seedLength` — `account_seed` is not 32 bytes.
/// * [`FfiError::Reach`] `scopeIdTooLong` / `scopeIdNotEmpty` /
///   `scopeIdEmpty` — the scope does not fit the class (empty for `Contact`
///   and `Fof`, 1 to 64 bytes otherwise).
#[uniffi::export]
pub fn reach_seed(
    account_seed: Vec<u8>,
    class: FfiReachClass,
    scope_id: Vec<u8>,
    version: u32,
) -> Result<Vec<u8>, FfiError> {
    let account = seed32(&account_seed, "account seed")?;
    let seed = rust_reach_seed(&account, class.into(), &scope_id, version)?;
    Ok(seed.as_bytes().to_vec())
}

/// [`reach_seed`] wrapped as a grant, ready to hand out.
///
/// # Errors
///
/// Exactly [`reach_seed`]'s.
#[uniffi::export]
pub fn derive_reach_grant(
    account_seed: Vec<u8>,
    class: FfiReachClass,
    scope_id: Vec<u8>,
    version: u32,
) -> Result<FfiReachGrant, FfiError> {
    let account = seed32(&account_seed, "account seed")?;
    Ok(RustReachGrant::derive(&account, class.into(), &scope_id, version)?.into())
}

/// The 32-byte Ed25519 public key of a reach seed: what the owner registers
/// with the Delivery Service, and what a mailbox write is verified against.
///
/// # Errors
///
/// [`FfiError::Reach`] `seedLength` — `seed` is not 32 bytes.
#[uniffi::export]
pub fn reach_verifying_key(seed: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_reach_verifying_key(&seed32(&seed, "reach seed")?).to_vec())
}

/// Encode a grant as its 37-byte wire (`class || version || seed`). The
/// output holds the seed: seal it before it leaves the device.
///
/// # Errors
///
/// [`FfiError::Reach`] `seedLength` — the seed is not 32 bytes.
#[uniffi::export]
pub fn encode_reach_grant(grant: FfiReachGrant) -> Result<Vec<u8>, FfiError> {
    let rust = RustReachGrant::try_from(grant)?;
    Ok(rust.encode().to_vec())
}

/// Decode exactly [`reach_grant_len`] bytes, fail-closed.
///
/// # Errors
///
/// [`FfiError::Reach`] `badLength` / `unknownClass`.
#[uniffi::export]
pub fn decode_reach_grant(bytes: Vec<u8>) -> Result<FfiReachGrant, FfiError> {
    Ok(RustReachGrant::decode(&bytes)?.into())
}

/// Length of an account seed, a reach seed and a reach public key: 32.
#[uniffi::export]
#[must_use]
pub fn reach_seed_len() -> u32 {
    u32::try_from(REACH_SEED_LEN).unwrap_or(u32::MAX)
}

/// Wire length of a reach grant: 37.
#[uniffi::export]
#[must_use]
pub fn reach_grant_len() -> u32 {
    u32::try_from(REACH_GRANT_LEN).unwrap_or(u32::MAX)
}

/// Longest scope id a reach key is derived over: 64.
#[uniffi::export]
#[must_use]
pub fn max_reach_scope_id_len() -> u32 {
    u32::try_from(MAX_REACH_SCOPE_ID_LEN).unwrap_or(u32::MAX)
}

// ============================================================================
// Introductions — ADR-0060 §4 (contacts of contacts)
// ============================================================================

/// What an introduction record reveals to a searcher who opens it. Mirror of
/// [`lakko_contacts::introduction::IntroductionPayload`].
///
/// The payload is the introduced person's claim: bind the username only
/// after [`verify_username_binding`], and run the DS reach check before
/// showing the result (ADR-0060 §4).
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiIntroductionPayload {
    /// The introduced person's 32-byte identity (account) public key.
    pub identity_pk: Vec<u8>,
    /// Their 16-byte account id.
    pub account_id: Vec<u8>,
    /// Their display name, or `None`.
    pub display_name: Option<String>,
    /// Their canonical username, or `None`.
    pub username: Option<String>,
    /// The 153-byte discovery-signed username certificate, or `None`.
    pub username_certificate: Option<Vec<u8>>,
    /// Their `Fof` reach grant (secret).
    pub fof: FfiReachGrant,
}

impl From<RustIntroductionPayload> for FfiIntroductionPayload {
    fn from(p: RustIntroductionPayload) -> Self {
        Self {
            identity_pk: p.identity_pk.to_vec(),
            account_id: p.account_id.to_vec(),
            display_name: p.display_name,
            username: p.username,
            username_certificate: p.username_certificate,
            fof: p.fof.into(),
        }
    }
}

impl TryFrom<FfiIntroductionPayload> for RustIntroductionPayload {
    type Error = FfiError;

    fn try_from(p: FfiIntroductionPayload) -> Result<Self, FfiError> {
        let identity_pk =
            <[u8; IDENTITY_PK_LEN]>::try_from(p.identity_pk.as_slice()).map_err(|_| {
                FfiError::Introduction(format!(
                    "identityPkLength: identity key must be {IDENTITY_PK_LEN} bytes, got {}",
                    p.identity_pk.len()
                ))
            })?;
        let account_id =
            <[u8; ACCOUNT_ID_LEN]>::try_from(p.account_id.as_slice()).map_err(|_| {
                FfiError::Introduction(format!(
                    "accountIdLength: account id must be {ACCOUNT_ID_LEN} bytes, got {}",
                    p.account_id.len()
                ))
            })?;
        Ok(Self {
            identity_pk,
            account_id,
            display_name: p.display_name,
            username: p.username,
            username_certificate: p.username_certificate,
            fof: p.fof.try_into()?,
        })
    }
}

fn oprf64(bytes: &[u8]) -> Result<[u8; OPRF_OUTPUT_LEN], FfiError> {
    <[u8; OPRF_OUTPUT_LEN]>::try_from(bytes).map_err(|_| {
        FfiError::Introduction(format!(
            "oprfLength: an OPRF output must be {OPRF_OUTPUT_LEN} bytes, got {}",
            bytes.len()
        ))
    })
}

/// Build one introduction record findable by each search term, supplied as
/// its 64-byte server OPRF output: the username's (when the owner has one)
/// and one per `nameSearchTerms` entry, each blinded with the matching
/// `OprfBlinder` constructor. The order is kept. Returns the record wire.
///
/// # Errors
///
/// * [`FfiError::Introduction`] `oprfLength` / `identityPkLength` /
///   `accountIdLength` — an argument of the wrong length.
/// * [`FfiError::Introduction`] `noTerms` / `tooManyTerms` / `duplicateTag` /
///   `certificate.length` / `certificate.withoutUsername` / `grant.notFof`.
/// * The typed `DisplayName*` / `Username*` variants for the payload fields.
#[uniffi::export]
pub fn compose_introduction(
    payload: FfiIntroductionPayload,
    term_oprfs: Vec<Vec<u8>>,
    issued_at_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    let payload = RustIntroductionPayload::try_from(payload)?;
    let terms = term_oprfs
        .iter()
        .map(|t| oprf64(t))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rust_compose_introduction(&payload, &terms, issued_at_ms)?)
}

/// The 32-byte tags of a record wire, to match against [`fof_tag`] without
/// opening.
///
/// # Errors
///
/// [`FfiError::Introduction`] for a malformed record.
#[uniffi::export]
pub fn introduction_tags(record: Vec<u8>) -> Result<Vec<Vec<u8>>, FfiError> {
    Ok(rust_introduction_tags(&record)?
        .into_iter()
        .map(|t| t.to_vec())
        .collect())
}

/// The tag a record entry for one 64-byte OPRF output carries.
///
/// # Errors
///
/// [`FfiError::Introduction`] `oprfLength`.
#[uniffi::export]
pub fn fof_tag(oprf_output: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_fof_tag(&oprf64(&oprf_output)?).to_vec())
}

/// Open a record wire with one 64-byte OPRF output. `None` when no entry
/// matches (not this person, or a padding dummy).
///
/// # Errors
///
/// * [`FfiError::Introduction`] `open` — a tag matched but the record failed
///   to authenticate: a forged or corrupted record, never a miss.
/// * [`FfiError::Introduction`] for a malformed record or payload.
#[uniffi::export]
pub fn open_introduction(
    record: Vec<u8>,
    oprf_output: Vec<u8>,
) -> Result<Option<FfiIntroductionPayload>, FfiError> {
    Ok(rust_open_introduction(&record, &oprf64(&oprf_output)?)?.map(Into::into))
}

/// Wrap one record as an introduction offer (owner to a contact, sent in
/// their 1:1 conversation).
///
/// # Errors
///
/// [`FfiError::Introduction`] for a malformed record — an offer never
/// carries a record a receiver would refuse.
#[uniffi::export]
pub fn compose_introduction_offer(record: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_compose_introduction_offer(&record)?)
}

/// Strictly decode an introduction offer, returning the record wire.
///
/// **Accept an offer only from the other participant of a 1:1
/// conversation**; keep the newest per contact.
///
/// # Errors
///
/// [`FfiError::Introduction`] `wrongDomain` / `unknownVersion` or a record
/// error.
#[uniffi::export]
pub fn decode_introduction_offer(plaintext: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(rust_decode_introduction_offer(&plaintext)?.encode()?)
}

/// A contact's bundle of introduction records, decoded. Mirror of
/// [`lakko_contacts::introduction::IntroductionsBundle`], records as wires.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiIntroductionsBundle {
    /// Sender-clock UNIX ms; orders bundles from one contact via
    /// [`bundle_is_newer`].
    pub updated_at_ms: u64,
    /// The record wires, padding included (a size-bucket count of them).
    pub records: Vec<Vec<u8>>,
}

fn bundle_to_ffi(b: RustIntroductionsBundle) -> Result<FfiIntroductionsBundle, FfiError> {
    Ok(FfiIntroductionsBundle {
        updated_at_ms: b.updated_at_ms,
        records: b
            .records
            .iter()
            .map(RustIntroductionRecord::encode)
            .collect::<Result<_, _>>()?,
    })
}

/// Build a bundle from the real record wires this device holds (the newest
/// per contact who offered one): padded to the next size bucket with
/// dummies, shuffled, encoded. The bundle goes to each contact in their 1:1
/// conversation; it is never re-relayed.
///
/// # Errors
///
/// [`FfiError::Introduction`] `tooManyRecords` or a record error.
#[uniffi::export]
pub fn compose_introductions_bundle(
    records: Vec<Vec<u8>>,
    updated_at_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    let decoded = records
        .iter()
        .map(|r| RustIntroductionRecord::decode(r))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rust_compose_bundle(&decoded, updated_at_ms)?)
}

/// Strictly decode an introductions bundle.
///
/// # Errors
///
/// [`FfiError::Introduction`] `wrongDomain` / `badRecordCount` /
/// `recordLength` or a record error.
#[uniffi::export]
pub fn decode_introductions_bundle(plaintext: Vec<u8>) -> Result<FfiIntroductionsBundle, FfiError> {
    bundle_to_ffi(rust_decode_introductions(&plaintext)?)
}

pub(crate) fn introductions_to_ffi(
    b: RustIntroductionsBundle,
) -> Result<FfiIntroductionsBundle, FfiError> {
    bundle_to_ffi(b)
}

/// `true` when a bundle stamped `incoming_updated_at_ms` must replace the
/// stored one from the **same contact**. Strictly greater wins.
#[uniffi::export]
#[must_use]
pub fn bundle_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_bundle_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Most search terms one record carries (64 name terms plus the username).
#[uniffi::export]
#[must_use]
pub fn max_introduction_terms() -> u32 {
    u32::try_from(MAX_INTRODUCTION_TERMS).unwrap_or(u32::MAX)
}

/// Most records one bundle carries, padding included.
#[uniffi::export]
#[must_use]
pub fn max_bundle_records() -> u32 {
    u32::try_from(MAX_BUNDLE_RECORDS).unwrap_or(u32::MAX)
}

/// The record counts a bundle may carry.
#[uniffi::export]
#[must_use]
pub fn bundle_size_buckets() -> Vec<u32> {
    BUNDLE_SIZE_BUCKETS
        .iter()
        .map(|&b| u32::try_from(b).unwrap_or(u32::MAX))
        .collect()
}

/// Length of a server OPRF output: 64.
#[uniffi::export]
#[must_use]
pub fn introduction_oprf_output_len() -> u32 {
    u32::try_from(OPRF_OUTPUT_LEN).unwrap_or(u32::MAX)
}

/// Length of a record tag: 32.
#[uniffi::export]
#[must_use]
pub fn fof_tag_len() -> u32 {
    u32::try_from(FOF_TAG_LEN).unwrap_or(u32::MAX)
}

// ============================================================================
// The setting — ADR-0060 §7, UX sections 1 and 3
// ============================================================================

/// The three positions (UX section 1).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiVisibilityMode {
    /// No one finds the person by username or name.
    Nobody,
    /// The parts that are on.
    Some,
    /// Anyone finds the person.
    Everyone,
}

impl From<FfiVisibilityMode> for VisibilityMode {
    fn from(m: FfiVisibilityMode) -> Self {
        match m {
            FfiVisibilityMode::Nobody => Self::Nobody,
            FfiVisibilityMode::Some => Self::Some,
            FfiVisibilityMode::Everyone => Self::Everyone,
        }
    }
}

impl From<VisibilityMode> for FfiVisibilityMode {
    fn from(m: VisibilityMode) -> Self {
        match m {
            VisibilityMode::Nobody => Self::Nobody,
            VisibilityMode::Some => Self::Some,
            VisibilityMode::Everyone => Self::Everyone,
        }
    }
}

/// All groups and channels, or only the chosen ones (UX 2.2.1 / 2.2.2).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiGroupsForm {
    /// Every group and channel the person is in.
    All,
    /// Only the ones in the chosen list.
    Chosen,
}

impl From<FfiGroupsForm> for GroupsForm {
    fn from(f: FfiGroupsForm) -> Self {
        match f {
            FfiGroupsForm::All => Self::All,
            FfiGroupsForm::Chosen => Self::Chosen,
        }
    }
}

impl From<GroupsForm> for FfiGroupsForm {
    fn from(f: GroupsForm) -> Self {
        match f {
            GroupsForm::All => Self::All,
            GroupsForm::Chosen => Self::Chosen,
        }
    }
}

/// Group or channel.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiScopeKind {
    /// A group (MLS group id).
    Group,
    /// A channel (channel id).
    Channel,
}

/// One group or channel.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiScopeId {
    /// Group or channel.
    pub kind: FfiScopeKind,
    /// The id, 1 to 64 bytes.
    pub id: Vec<u8>,
}

impl From<FfiScopeId> for RustScopeId {
    fn from(s: FfiScopeId) -> Self {
        Self {
            kind: match s.kind {
                FfiScopeKind::Group => ScopeKind::Group,
                FfiScopeKind::Channel => ScopeKind::Channel,
            },
            id: s.id,
        }
    }
}

impl From<RustScopeId> for FfiScopeId {
    fn from(s: RustScopeId) -> Self {
        Self {
            kind: match s.kind {
                ScopeKind::Group => FfiScopeKind::Group,
                ScopeKind::Channel => FfiScopeKind::Channel,
            },
            id: s.id,
        }
    }
}

/// The "who can find me" setting (UX sections 1 and 3). Mirror of
/// [`lakko_contacts::visibility::VisibilitySetting`]. Edit it only through
/// the Settings transitions in `ui_rules.rs`, then stamp `updated_at_ms`
/// and send it over the DeviceGroup.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiVisibilitySetting {
    /// The position.
    pub mode: FfiVisibilityMode,
    /// Part 2.1: contacts of my contacts. Only while `mode` is `Some`.
    pub contacts_of_contacts: bool,
    /// Part 2.2 and its form; `None` when off. Only while `mode` is `Some`.
    pub groups: Option<FfiGroupsForm>,
    /// The groups and channels switched on under "only the ones I choose".
    /// Kept whatever the form.
    pub chosen: Vec<FfiScopeId>,
    /// Memory: whether part 2.1 was on when the person last left `Some`.
    pub last_contacts_of_contacts: bool,
    /// Memory: part 2.2's form when the person last left `Some`.
    pub last_groups: Option<FfiGroupsForm>,
    /// Setter-clock UNIX ms; orders settings via [`visibility_is_newer`].
    pub updated_at_ms: u64,
}

impl From<FfiVisibilitySetting> for RustVisibilitySetting {
    fn from(s: FfiVisibilitySetting) -> Self {
        Self {
            mode: s.mode.into(),
            contacts_of_contacts: s.contacts_of_contacts,
            groups: s.groups.map(Into::into),
            chosen: s.chosen.into_iter().map(Into::into).collect(),
            last_contacts_of_contacts: s.last_contacts_of_contacts,
            last_groups: s.last_groups.map(Into::into),
            updated_at_ms: s.updated_at_ms,
        }
    }
}

impl From<RustVisibilitySetting> for FfiVisibilitySetting {
    fn from(s: RustVisibilitySetting) -> Self {
        Self {
            mode: s.mode.into(),
            contacts_of_contacts: s.contacts_of_contacts,
            groups: s.groups.map(Into::into),
            chosen: s.chosen.into_iter().map(Into::into).collect(),
            last_contacts_of_contacts: s.last_contacts_of_contacts,
            last_groups: s.last_groups.map(Into::into),
            updated_at_ms: s.updated_at_ms,
        }
    }
}

/// The default for a new account: Some people, with contacts of contacts and
/// all groups and channels on; `updated_at_ms` 0.
#[uniffi::export]
#[must_use]
pub fn default_visibility_setting() -> FfiVisibilitySetting {
    RustVisibilitySetting::default_for_new_account().into()
}

/// Compose the setting envelope, sent over the owner's DeviceGroup.
///
/// # Errors
///
/// [`FfiError::Visibility`] with the setting rule the value breaks
/// (`someWithNoPart`, `partsOutsideSome`, `tooManyChosen`, `duplicateScope`,
/// `scopeIdLength`).
#[uniffi::export]
pub fn compose_visibility_envelope(setting: FfiVisibilitySetting) -> Result<Vec<u8>, FfiError> {
    Ok(rust_compose_visibility(&setting.into())?)
}

/// Strictly decode a setting envelope. **Accept one only on the owner's own
/// DeviceGroup**, and keep it only when [`visibility_is_newer`].
///
/// # Errors
///
/// [`FfiError::Visibility`] for a wrong domain, a malformed or an invalid
/// setting.
#[uniffi::export]
pub fn decode_visibility_envelope(plaintext: Vec<u8>) -> Result<FfiVisibilitySetting, FfiError> {
    Ok(rust_decode_visibility(&plaintext)?.into())
}

/// `true` when a setting stamped `incoming_updated_at_ms` must replace the
/// stored one. Strictly greater wins.
#[uniffi::export]
#[must_use]
pub fn visibility_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_visibility_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Whether the members of `scope` can find the person: Everyone, or Some
/// people with groups on and either all or this one chosen. A permission,
/// not a delivery decision.
#[uniffi::export]
#[must_use]
pub fn allows_scope(setting: FfiVisibilitySetting, scope: FfiScopeId) -> bool {
    RustVisibilitySetting::from(setting).allows_scope(&scope.into())
}

/// Whether contacts of contacts can find the person: Everyone, or Some
/// people with part 2.1 on.
#[uniffi::export]
#[must_use]
pub fn allows_contacts_of_contacts(setting: FfiVisibilitySetting) -> bool {
    RustVisibilitySetting::from(setting).allows_contacts_of_contacts()
}

/// Most groups and channels the chosen list holds.
#[uniffi::export]
#[must_use]
pub fn max_chosen_scopes() -> u32 {
    u32::try_from(MAX_CHOSEN_SCOPES).unwrap_or(u32::MAX)
}

/// Longest group or channel id in the chosen list.
#[uniffi::export]
#[must_use]
pub fn max_visibility_scope_id_len() -> u32 {
    u32::try_from(MAX_SCOPE_ID_LEN).unwrap_or(u32::MAX)
}

// ============================================================================
// The reach state — every reach key version counter
// ============================================================================

/// One reach key's version counter.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiReachStateEntry {
    /// The class.
    pub class: FfiReachClass,
    /// The scope, shaped per the class (empty for `Contact` / `Fof`).
    pub scope_id: Vec<u8>,
    /// The current key version.
    pub version: u32,
}

/// Every reach key version counter the owner's devices share. Mirror of
/// [`lakko_contacts::visibility::ReachState`].
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiReachState {
    /// One entry per `(class, scope_id)`.
    pub entries: Vec<FfiReachStateEntry>,
    /// Setter-clock UNIX ms; orders states via [`reach_state_is_newer`].
    pub updated_at_ms: u64,
}

impl From<FfiReachState> for RustReachState {
    fn from(s: FfiReachState) -> Self {
        Self {
            entries: s
                .entries
                .into_iter()
                .map(|e| RustReachStateEntry {
                    class: e.class.into(),
                    scope_id: e.scope_id,
                    version: e.version,
                })
                .collect(),
            updated_at_ms: s.updated_at_ms,
        }
    }
}

impl From<RustReachState> for FfiReachState {
    fn from(s: RustReachState) -> Self {
        Self {
            entries: s
                .entries
                .into_iter()
                .map(|e| FfiReachStateEntry {
                    class: e.class.into(),
                    scope_id: e.scope_id,
                    version: e.version,
                })
                .collect(),
            updated_at_ms: s.updated_at_ms,
        }
    }
}

/// Compose the reach-state envelope, sent over the owner's DeviceGroup.
///
/// # Errors
///
/// [`FfiError::Visibility`] `tooManyEntries` / `duplicateEntry` /
/// `reach.*` (a scope that does not fit its class).
#[uniffi::export]
pub fn compose_reach_state_envelope(state: FfiReachState) -> Result<Vec<u8>, FfiError> {
    Ok(rust_compose_reach_state(&state.into())?)
}

/// Strictly decode a reach-state envelope. Same acceptance rules as the
/// setting, ordered by [`reach_state_is_newer`].
///
/// # Errors
///
/// [`FfiError::Visibility`] for a wrong domain, a malformed or an invalid
/// state.
#[uniffi::export]
pub fn decode_reach_state_envelope(plaintext: Vec<u8>) -> Result<FfiReachState, FfiError> {
    Ok(rust_decode_reach_state(&plaintext)?.into())
}

/// `true` when a reach state stamped `incoming_updated_at_ms` must replace
/// the stored one. Strictly greater wins.
#[uniffi::export]
#[must_use]
pub fn reach_state_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_reach_state_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// The version of `(class, scope_id)` in `state`, or `None`.
#[uniffi::export]
#[must_use]
pub fn reach_state_version_of(
    state: FfiReachState,
    class: FfiReachClass,
    scope_id: Vec<u8>,
) -> Option<u32> {
    RustReachState::from(state).version_of(class.into(), &scope_id)
}

/// Most entries a reach state holds.
#[uniffi::export]
#[must_use]
pub fn max_reach_state_entries() -> u32 {
    u32::try_from(MAX_REACH_STATE_ENTRIES).unwrap_or(u32::MAX)
}

// ============================================================================
// Username certificate — ADR-0060 §5 (verify only)
// ============================================================================

/// The authenticated claims of a verified username certificate.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiUsernameCertificate {
    /// The certified 64-byte OPRF output of the username.
    pub oprf_hash: Vec<u8>,
    /// The 16-byte account the username is registered to.
    pub account_id: Vec<u8>,
    /// When discovery issued it, Unix ms. Informational: a certificate is
    /// permanent.
    pub issued_at_unix_millis: i64,
}

impl From<RustUsernameCertificate> for FfiUsernameCertificate {
    fn from(c: RustUsernameCertificate) -> Self {
        Self {
            oprf_hash: c.oprf_hash().to_vec(),
            account_id: c.account_id().to_vec(),
            issued_at_unix_millis: c.issued_at_unix_millis(),
        }
    }
}

fn cert_arg(code: &str, what: &str, want: usize, got: usize) -> FfiError {
    FfiError::UsernameCertificate(format!("{code}: {what} must be {want} bytes, got {got}"))
}

/// Verify a claimed username: the certificate's signature under the pinned
/// discovery key, its binding to `expected_oprf_hash` (the OPRF output the
/// viewer computed over the CLAIMED username) and to `expected_account_id`
/// (the account the profile arrived from). No freshness check — usernames are
/// immutable. Show the username only on success (invariant #4).
///
/// # Errors
///
/// [`FfiError::UsernameCertificate`] with `badLength` / `badVersion` /
/// `badSignature` / `usernameMismatch` / `accountMismatch`, or
/// `badVerifyingKey` / `oprfHashLength` / `accountIdLength` for an argument.
#[uniffi::export]
pub fn verify_username_binding(
    certificate: Vec<u8>,
    verifying_key: Vec<u8>,
    expected_oprf_hash: Vec<u8>,
    expected_account_id: Vec<u8>,
) -> Result<FfiUsernameCertificate, FfiError> {
    let verifying = VerifyingKey::from_bytes(&verifying_key).map_err(|_| {
        FfiError::UsernameCertificate(
            "badVerifyingKey: the pinned discovery key is not an Ed25519 key".to_string(),
        )
    })?;
    let oprf = <[u8; OPRF_HASH_LEN]>::try_from(expected_oprf_hash.as_slice()).map_err(|_| {
        cert_arg(
            "oprfHashLength",
            "OPRF hash",
            OPRF_HASH_LEN,
            expected_oprf_hash.len(),
        )
    })?;
    let account =
        <[u8; ACCOUNT_ID_LEN]>::try_from(expected_account_id.as_slice()).map_err(|_| {
            cert_arg(
                "accountIdLength",
                "account id",
                ACCOUNT_ID_LEN,
                expected_account_id.len(),
            )
        })?;
    Ok(rust_verify_username_binding(&certificate, &verifying, &oprf, &account)?.into())
}

/// Wire length of a username certificate: 153.
#[uniffi::export]
#[must_use]
pub fn username_certificate_len() -> u32 {
    u32::try_from(lakko_attest::USERNAME_CERT_LEN).unwrap_or(u32::MAX)
}

// ============================================================================
// Committer delegations — ADR-0060 §6
// ============================================================================

impl From<CommitterDelegationError> for FfiError {
    fn from(e: CommitterDelegationError) -> Self {
        Self::CommitterDelegation(format!("{}: {e}", e.code()))
    }
}

impl From<CommitterDelegationCheckError> for FfiError {
    fn from(e: CommitterDelegationCheckError) -> Self {
        Self::CommitterDelegation(format!("{}: {e}", e.code()))
    }
}

fn delegation_error(e: &DelegationError) -> FfiError {
    let code = match e {
        DelegationError::UnknownVersion => "delegationUnknownVersion",
        _ => "delegationMalformed",
    };
    FfiError::CommitterDelegation(format!("{code}: {e}"))
}

/// A proposer's committer delegations (ADR-0060 §6). Mirror of
/// [`lakko_contacts::committer_delegation::CommitterDelegationEnvelope`].
///
/// Under ADR-0045 the member who proposes an `Add` may not be the one who
/// commits it, and the added person's mailbox is closed. So the proposer
/// posts, in the group beside its proposal, one delegation per committer
/// candidate (the owner and each admin), each signed with the proposer's
/// reach key for the added person's mailbox ([`sign_delegation_blob`]) and
/// naming that candidate's account signature key as delegate. A delegation
/// lives at most 24 hours: **re-send a fresh envelope while the proposal is
/// pending**, and whenever the candidate set changes.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiCommitterDelegationEnvelope {
    /// The added person's 32-byte account signature key; every delegation
    /// must name its mailbox.
    pub added_account_key: Vec<u8>,
    /// 1 to [`max_committer_delegations`] raw 185-byte delegation blobs.
    pub delegations: Vec<Vec<u8>>,
}

impl From<CommitterDelegationEnvelope> for FfiCommitterDelegationEnvelope {
    fn from(e: CommitterDelegationEnvelope) -> Self {
        Self {
            added_account_key: e.added_account_key.to_vec(),
            delegations: e.delegations.iter().map(|d| d.to_vec()).collect(),
        }
    }
}

impl TryFrom<FfiCommitterDelegationEnvelope> for CommitterDelegationEnvelope {
    type Error = FfiError;

    fn try_from(e: FfiCommitterDelegationEnvelope) -> Result<Self, FfiError> {
        let added_account_key = <[u8; ADDED_ACCOUNT_KEY_LEN]>::try_from(
            e.added_account_key.as_slice(),
        )
        .map_err(|_| {
            reach_arg(
                "keyLength",
                "added account key",
                ADDED_ACCOUNT_KEY_LEN,
                e.added_account_key.len(),
            )
        })?;
        let delegations = e
            .delegations
            .iter()
            .map(|d| {
                <[u8; COMMITTER_DELEGATION_BLOB_LEN]>::try_from(d.as_slice()).map_err(|_| {
                    FfiError::CommitterDelegation(format!(
                        "delegationMalformed: a delegation must be \
                         {COMMITTER_DELEGATION_BLOB_LEN} bytes, got {}",
                        d.len()
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            added_account_key,
            delegations,
        })
    }
}

/// One delegation, decoded (structure only; the Delivery Service checks the
/// signature and the expiry when it is used).
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiDelegation {
    /// The mailbox pseudonym the write may go to (32 bytes).
    pub mailbox: Vec<u8>,
    /// The key that signs the delegated request (32 bytes).
    pub delegate_pk: Vec<u8>,
    /// When the delegation stops being honoured, unix milliseconds.
    pub expires_at_ms: u64,
    /// The 16-byte nonce the DS consumes on use.
    pub nonce: Vec<u8>,
    /// The reach public key that signed the delegation (32 bytes).
    pub issuer_pk: Vec<u8>,
}

impl From<&Delegation> for FfiDelegation {
    fn from(d: &Delegation) -> Self {
        Self {
            mailbox: d.mailbox.to_vec(),
            delegate_pk: d.delegate_pk.to_vec(),
            expires_at_ms: d.expires_at_ms,
            nonce: d.nonce.to_vec(),
            issuer_pk: d.issuer_pk.to_vec(),
        }
    }
}

/// Compose a committer-delegation envelope: the plaintext of an MLS
/// application message the proposer sends in the group beside its `Add`
/// proposal (ADR-0060 §6). Structural checks only; build each blob with
/// [`sign_delegation_blob`].
///
/// # Errors
///
/// [`FfiError::CommitterDelegation`] `badCount` (none, or more than
/// [`max_committer_delegations`]) or `delegationMalformed` (a blob that is
/// not 185 bytes); [`FfiError::Reach`] `keyLength` for an added account key
/// that is not 32 bytes.
#[uniffi::export]
pub fn compose_committer_delegation_envelope(
    envelope: FfiCommitterDelegationEnvelope,
) -> Result<Vec<u8>, FfiError> {
    let envelope = CommitterDelegationEnvelope::try_from(envelope)?;
    Ok(rust_compose_committer_delegation(&envelope)?)
}

/// Strictly decode a committer-delegation envelope (structure only). The
/// dispatcher already returns it as `CommitterDelegation`; this is for a
/// caller that holds the plaintext alone.
///
/// # Errors
///
/// [`FfiError::CommitterDelegation`] `wrongDomain` / `unknownVersion` /
/// `badCount` / `badLength`.
#[uniffi::export]
pub fn decode_committer_delegation_envelope(
    plaintext: Vec<u8>,
) -> Result<FfiCommitterDelegationEnvelope, FfiError> {
    Ok(rust_decode_committer_delegation(&plaintext)?.into())
}

/// Parse and check every delegation of an envelope (ADR-0060 §6): each blob
/// parses, names the added person's mailbox, and no two name the same
/// delegate. Any failure refuses the whole envelope — keep the previous one.
///
/// # Errors
///
/// [`FfiError::CommitterDelegation`] `badAddedAccountKey` / `malformed` /
/// `wrongMailbox` / `duplicateDelegate`, plus the argument errors of
/// [`compose_committer_delegation_envelope`].
#[uniffi::export]
pub fn check_committer_delegations(
    envelope: FfiCommitterDelegationEnvelope,
) -> Result<Vec<FfiDelegation>, FfiError> {
    let envelope = CommitterDelegationEnvelope::try_from(envelope)?;
    Ok(rust_check_committer_delegations(&envelope)?
        .iter()
        .map(FfiDelegation::from)
        .collect())
}

/// The committer's side of ADR-0060 §6: after its sweep added the envelope's
/// person, the delegation naming `own_account_key` (this device's account
/// signature key), as the `x-lakko-ds-delegation` header value to send with
/// the Welcome's mailbox write — signed with
/// `DsProofSigner.forReach(accountIdentity ed25519 seed)`, the key the
/// delegation names. `None` when no delegation names this account (another
/// candidate commits, or the proposer has not re-sent since this device
/// became a candidate). The envelope is checked first
/// ([`check_committer_delegations`]).
///
/// Use a delegation only from the newest envelope kept per
/// `(MLS sender resolved at receipt, added_account_key)`, and only for the
/// Welcome of a sweep that added that person.
///
/// # Errors
///
/// Every error of [`check_committer_delegations`]; [`FfiError::Reach`]
/// `keyLength` for an own key that is not 32 bytes.
#[uniffi::export]
pub fn committer_delegation_for(
    envelope: FfiCommitterDelegationEnvelope,
    own_account_key: Vec<u8>,
) -> Result<Option<String>, FfiError> {
    let own =
        <[u8; ADDED_ACCOUNT_KEY_LEN]>::try_from(own_account_key.as_slice()).map_err(|_| {
            reach_arg(
                "keyLength",
                "own account key",
                ADDED_ACCOUNT_KEY_LEN,
                own_account_key.len(),
            )
        })?;
    let envelope = CommitterDelegationEnvelope::try_from(envelope)?;
    let parsed = rust_check_committer_delegations(&envelope)?;
    Ok(rust_delegation_for(&parsed, &own).map(Delegation::to_header_value))
}

/// Decode one raw 185-byte delegation blob (structure only).
///
/// # Errors
///
/// [`FfiError::CommitterDelegation`] `delegationMalformed` /
/// `delegationUnknownVersion`.
#[uniffi::export]
pub fn decode_delegation(blob: Vec<u8>) -> Result<FfiDelegation, FfiError> {
    Delegation::decode(&blob)
        .map(|d| FfiDelegation::from(&d))
        .map_err(|e| delegation_error(&e))
}

/// The `x-lakko-ds-delegation` header value of a raw delegation blob.
///
/// # Errors
///
/// As [`decode_delegation`].
#[uniffi::export]
pub fn delegation_header_value(blob: Vec<u8>) -> Result<String, FfiError> {
    Delegation::decode(&blob)
        .map(|d| d.to_header_value())
        .map_err(|e| delegation_error(&e))
}

/// Most delegations one envelope carries: 16.
#[uniffi::export]
#[must_use]
pub fn max_committer_delegations() -> u32 {
    u32::try_from(MAX_COMMITTER_DELEGATIONS).unwrap_or(u32::MAX)
}

/// Length of one delegation blob: 185.
#[uniffi::export]
#[must_use]
pub fn committer_delegation_blob_len() -> u32 {
    u32::try_from(COMMITTER_DELEGATION_BLOB_LEN).unwrap_or(u32::MAX)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_attest::username_cert::UsernameCertificate;
    use lakko_crypto::signing::SigningKey;

    const ACCOUNT_SEED: [u8; 32] = [0x42; 32];

    /// ADR-0060 §6 end to end through this surface: the proposer signs one
    /// blob per candidate, the envelope rides the dispatcher, and each
    /// candidate finds exactly its own delegation as a header the DS checks.
    #[test]
    fn a_committer_finds_its_own_delegation_through_the_dispatcher() {
        let account = |b: u8| SigningKey::from_seed(&[b; 32]).verifying_key().to_bytes();
        let added = account(9);
        let mailbox = lakko_ds_proof::mailbox_pseudonym(&VerifyingKey::from_bytes(&added).unwrap());
        let (owner, admin) = (account(1), account(2));
        let expires = 1_000;
        let blob = |delegate: [u8; 32], n: u8| {
            crate::ds_proof::sign_delegation_blob(
                vec![0x44; 32],
                mailbox.to_vec(),
                delegate.to_vec(),
                expires,
                vec![n; 16],
            )
            .unwrap()
        };
        let envelope = FfiCommitterDelegationEnvelope {
            added_account_key: added.to_vec(),
            delegations: vec![blob(owner, 1), blob(admin, 2)],
        };
        let wire = compose_committer_delegation_envelope(envelope.clone()).unwrap();
        let dispatched = crate::contacts::dispatch_incoming_envelope(wire.clone()).unwrap();
        assert_eq!(
            dispatched.kind,
            crate::contacts::FfiIncomingEnvelopeKind::CommitterDelegation
        );
        assert_eq!(dispatched.committer_delegation, Some(envelope.clone()));
        assert!(dispatched.payload.is_empty());
        assert_eq!(
            decode_committer_delegation_envelope(wire).unwrap(),
            envelope
        );

        let checked = check_committer_delegations(envelope.clone()).unwrap();
        assert_eq!(checked.len(), 2);
        assert_eq!(checked[1].delegate_pk, admin.to_vec());
        let header = committer_delegation_for(envelope.clone(), admin.to_vec())
            .unwrap()
            .unwrap();
        assert_eq!(
            header,
            delegation_header_value(envelope.delegations[1].clone()).unwrap()
        );
        let parsed = Delegation::parse_header_value(&header).unwrap();
        assert!(lakko_ds_proof::verify_delegation(&parsed, &mailbox, expires - 10).is_ok());
        assert_eq!(
            committer_delegation_for(envelope.clone(), account(3).to_vec()).unwrap(),
            None
        );
        assert_eq!(
            decode_delegation(envelope.delegations[0].clone())
                .unwrap()
                .delegate_pk,
            owner
        );

        // A delegation for another mailbox refuses the whole envelope.
        let mut bad = envelope;
        bad.delegations[1] = crate::ds_proof::sign_delegation_blob(
            vec![0x44; 32],
            vec![0x11; 32],
            admin.to_vec(),
            expires,
            vec![3; 16],
        )
        .unwrap();
        let err = committer_delegation_for(bad, owner.to_vec()).unwrap_err();
        assert!(matches!(err, FfiError::CommitterDelegation(_)));
        assert_eq!(code_of(&err), "wrongMailbox");
        let err = compose_committer_delegation_envelope(FfiCommitterDelegationEnvelope {
            added_account_key: added.to_vec(),
            delegations: vec![],
        })
        .unwrap_err();
        assert_eq!(code_of(&err), "badCount");
        assert_eq!(max_committer_delegations(), 16);
        assert_eq!(committer_delegation_blob_len(), 185);
    }

    fn code_of(e: &FfiError) -> String {
        e.to_string()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn hex(bytes: &[u8]) -> String {
        use core::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    /// The core's golden vector (`lakko-contacts` reach.rs), reproduced
    /// through this surface. The other FFI surface pins the same literals, so
    /// iOS, Android and web derive the same reach key for the same account.
    #[test]
    fn reach_seed_reproduces_the_core_golden() {
        let seed = reach_seed(
            ACCOUNT_SEED.to_vec(),
            FfiReachClass::Group,
            b"group-1".to_vec(),
            3,
        )
        .unwrap();
        assert_eq!(
            hex(&seed),
            "00cad2ad9a1885862187f17ef75e33818653b1b54b9a480dad919d3d3d9d851e"
        );
        assert_eq!(
            hex(&reach_verifying_key(seed.clone()).unwrap()),
            "e7c2e2aa6ff74083318d5f6b1dfa7939c3cc074549376d9e91c5592a67a35e8f"
        );
    }

    #[test]
    fn reach_seed_matches_the_core_and_grants_round_trip() {
        let seed = reach_seed(ACCOUNT_SEED.to_vec(), FfiReachClass::Group, vec![7; 8], 3).unwrap();
        let core =
            rust_reach_seed(&ACCOUNT_SEED, ReachClass::Group, &[7; 8], 3).expect("core derives");
        assert_eq!(seed, core.as_bytes().to_vec());
        assert_eq!(
            reach_verifying_key(seed.clone()).unwrap(),
            rust_reach_verifying_key(core.as_bytes()).to_vec()
        );

        let grant =
            derive_reach_grant(ACCOUNT_SEED.to_vec(), FfiReachClass::Group, vec![7; 8], 3).unwrap();
        assert_eq!(grant.seed, seed);
        let wire = encode_reach_grant(grant.clone()).unwrap();
        assert_eq!(wire.len(), reach_grant_len() as usize);
        assert_eq!(decode_reach_grant(wire).unwrap(), grant);
        assert!(
            format!("{grant:?}").contains("<redacted>"),
            "Debug redacts the seed"
        );
        assert_eq!(reach_seed_len(), 32);
        assert_eq!(max_reach_scope_id_len(), 64);
    }

    #[test]
    fn reach_refusals_carry_their_code() {
        let e = reach_seed(ACCOUNT_SEED.to_vec(), FfiReachClass::Contact, vec![1], 1).unwrap_err();
        assert!(matches!(&e, FfiError::Reach(_)));
        assert_eq!(code_of(&e), "scopeIdNotEmpty");
        let e = reach_seed(ACCOUNT_SEED.to_vec(), FfiReachClass::Group, vec![], 1).unwrap_err();
        assert_eq!(code_of(&e), "scopeIdEmpty");
        let e = reach_seed(vec![0; 31], FfiReachClass::Fof, vec![], 1).unwrap_err();
        assert_eq!(code_of(&e), "seedLength");
        let e = decode_reach_grant(vec![0; 36]).unwrap_err();
        assert_eq!(code_of(&e), "badLength");
        let mut wire = vec![0u8; 37];
        wire[0] = 9;
        assert_eq!(
            code_of(&decode_reach_grant(wire).unwrap_err()),
            "unknownClass"
        );
    }

    fn payload() -> FfiIntroductionPayload {
        FfiIntroductionPayload {
            identity_pk: vec![1; 32],
            account_id: vec![2; 16],
            display_name: Some("Anna Weber".to_string()),
            username: Some("anna".to_string()),
            username_certificate: None,
            fof: derive_reach_grant(ACCOUNT_SEED.to_vec(), FfiReachClass::Fof, vec![], 1).unwrap(),
        }
    }

    #[test]
    fn introduction_round_trips_through_offer_and_bundle() {
        let terms = vec![vec![5u8; 64], vec![6u8; 64]];
        let record = compose_introduction(payload(), terms.clone(), 10).unwrap();
        let tags = introduction_tags(record.clone()).unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[1], fof_tag(terms[1].clone()).unwrap());
        assert_eq!(
            open_introduction(record.clone(), terms[0].clone()).unwrap(),
            Some(payload())
        );
        assert_eq!(
            open_introduction(record.clone(), vec![9; 64]).unwrap(),
            None
        );

        let offer = compose_introduction_offer(record.clone()).unwrap();
        assert_eq!(decode_introduction_offer(offer).unwrap(), record);

        let bundle = compose_introductions_bundle(vec![record.clone()], 4).unwrap();
        let decoded = decode_introductions_bundle(bundle).unwrap();
        assert_eq!(decoded.updated_at_ms, 4);
        assert_eq!(decoded.records, vec![record]);
        assert!(bundle_is_newer(5, 4) && !bundle_is_newer(4, 4));
        assert_eq!(bundle_size_buckets()[0], 1);
        assert_eq!(max_introduction_terms(), 65);
        assert_eq!(max_bundle_records(), 1024);
        assert_eq!(introduction_oprf_output_len(), 64);
        assert_eq!(fof_tag_len(), 32);
    }

    #[test]
    fn introduction_refusals_carry_their_code() {
        let e = compose_introduction(payload(), vec![], 1).unwrap_err();
        assert!(matches!(&e, FfiError::Introduction(_)));
        assert_eq!(code_of(&e), "noTerms");
        let e = compose_introduction(payload(), vec![vec![5; 64], vec![5; 64]], 1).unwrap_err();
        assert_eq!(code_of(&e), "duplicateTag");
        let e = compose_introduction(payload(), vec![vec![5; 63]], 1).unwrap_err();
        assert_eq!(code_of(&e), "oprfLength");
        let mut bad = payload();
        bad.fof.class = FfiReachClass::Contact;
        assert_eq!(
            code_of(&compose_introduction(bad, vec![vec![5; 64]], 1).unwrap_err()),
            "grant.notFof"
        );
        let mut bad = payload();
        bad.display_name = Some("a\nb".to_string());
        assert!(matches!(
            compose_introduction(bad, vec![vec![5; 64]], 1),
            Err(FfiError::DisplayNameLineBreak(_))
        ));
        assert_eq!(
            code_of(&decode_introduction_offer(b"nope".to_vec()).unwrap_err()),
            "wrongDomain"
        );
        assert_eq!(
            code_of(&decode_introductions_bundle(b"nope".to_vec()).unwrap_err()),
            "wrongDomain"
        );
    }

    #[test]
    fn visibility_and_reach_state_round_trip() {
        let mut setting = default_visibility_setting();
        assert_eq!(setting.mode, FfiVisibilityMode::Some);
        setting.updated_at_ms = 9;
        setting.groups = Some(FfiGroupsForm::Chosen);
        let chosen = FfiScopeId {
            kind: FfiScopeKind::Channel,
            id: vec![3; 16],
        };
        setting.chosen = vec![chosen.clone()];
        let wire = compose_visibility_envelope(setting.clone()).unwrap();
        assert_eq!(decode_visibility_envelope(wire).unwrap(), setting);
        assert!(allows_scope(setting.clone(), chosen));
        assert!(!allows_scope(
            setting.clone(),
            FfiScopeId {
                kind: FfiScopeKind::Group,
                id: vec![3; 16],
            }
        ));
        assert!(allows_contacts_of_contacts(setting.clone()));
        assert!(visibility_is_newer(10, 9) && !visibility_is_newer(9, 9));

        let state = FfiReachState {
            entries: vec![
                FfiReachStateEntry {
                    class: FfiReachClass::Contact,
                    scope_id: vec![],
                    version: 2,
                },
                FfiReachStateEntry {
                    class: FfiReachClass::Group,
                    scope_id: vec![1; 8],
                    version: 5,
                },
            ],
            updated_at_ms: 11,
        };
        let wire = compose_reach_state_envelope(state.clone()).unwrap();
        assert_eq!(decode_reach_state_envelope(wire).unwrap(), state);
        assert_eq!(
            reach_state_version_of(state.clone(), FfiReachClass::Group, vec![1; 8]),
            Some(5)
        );
        assert_eq!(
            reach_state_version_of(state, FfiReachClass::Fof, vec![]),
            None
        );
        assert!(reach_state_is_newer(12, 11) && !reach_state_is_newer(11, 11));
        assert_eq!(max_chosen_scopes(), 1024);
        assert_eq!(max_visibility_scope_id_len(), 64);
        assert_eq!(max_reach_state_entries(), 4096);
    }

    #[test]
    fn visibility_refusals_carry_their_code() {
        let mut setting = default_visibility_setting();
        setting.contacts_of_contacts = false;
        setting.groups = None;
        let e = compose_visibility_envelope(setting).unwrap_err();
        assert!(matches!(&e, FfiError::Visibility(_)));
        assert_eq!(code_of(&e), "someWithNoPart");
        let state = FfiReachState {
            entries: vec![FfiReachStateEntry {
                class: FfiReachClass::Group,
                scope_id: vec![],
                version: 1,
            }],
            updated_at_ms: 1,
        };
        assert_eq!(
            code_of(&compose_reach_state_envelope(state).unwrap_err()),
            "reach.scopeIdEmpty"
        );
        assert_eq!(
            code_of(&decode_visibility_envelope(b"x".to_vec()).unwrap_err()),
            "wrongDomain"
        );
    }

    #[test]
    fn username_binding_verifies_and_refuses_with_codes() {
        let key = SigningKey::from_seed(&[0x11; 32]);
        let cert = UsernameCertificate::new([0xAB; 64], [0xCD; 16], 1_700_000_000_000).sign(&key);
        let vk = key.verifying_key().to_bytes().to_vec();
        let ok = verify_username_binding(cert.to_vec(), vk.clone(), vec![0xAB; 64], vec![0xCD; 16])
            .unwrap();
        assert_eq!(ok.issued_at_unix_millis, 1_700_000_000_000);
        assert_eq!(ok.account_id, vec![0xCD; 16]);

        let cases = [
            (
                cert.to_vec(),
                vec![0xAC; 64],
                vec![0xCD; 16],
                "usernameMismatch",
            ),
            (
                cert.to_vec(),
                vec![0xAB; 64],
                vec![0xCE; 16],
                "accountMismatch",
            ),
            (
                cert[..152].to_vec(),
                vec![0xAB; 64],
                vec![0xCD; 16],
                "badLength",
            ),
            (
                cert.to_vec(),
                vec![0xAB; 63],
                vec![0xCD; 16],
                "oprfHashLength",
            ),
            (
                cert.to_vec(),
                vec![0xAB; 64],
                vec![0xCD; 15],
                "accountIdLength",
            ),
        ];
        for (c, h, a, code) in cases {
            let e = verify_username_binding(c, vk.clone(), h, a).unwrap_err();
            assert!(matches!(&e, FfiError::UsernameCertificate(_)));
            assert_eq!(code_of(&e), code);
        }
        let mut tampered = cert.to_vec();
        tampered[20] ^= 1;
        assert_eq!(
            code_of(
                &verify_username_binding(tampered, vk, vec![0xAB; 64], vec![0xCD; 16]).unwrap_err()
            ),
            "badSignature"
        );
        assert_eq!(username_certificate_len(), 153);
    }

    /// The code table is duplicated in `lakko-ffi-wasm` (the attest crate has
    /// no `code()`); both sides pin the same literals.
    #[test]
    fn username_cert_codes_are_frozen() {
        assert_eq!(
            username_cert_code(&UsernameCertError::BadLength { got: 1 }),
            "badLength"
        );
        assert_eq!(
            username_cert_code(&UsernameCertError::BadVersion(2)),
            "badVersion"
        );
        assert_eq!(
            username_cert_code(&UsernameCertError::BadSignature),
            "badSignature"
        );
        assert_eq!(
            username_cert_code(&UsernameCertError::UsernameMismatch),
            "usernameMismatch"
        );
        assert_eq!(
            username_cert_code(&UsernameCertError::AccountMismatch),
            "accountMismatch"
        );
    }
}
