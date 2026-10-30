// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of ADR-0060 "who can find me" — scoped discovery.
//!
//! Mirror of `lakko-ffi-uniffi/src/scoped_discovery.rs` over the same cores
//! (reach keys, introduction records and bundles, the setting and the reach
//! state, username-certificate verification). Same function names; the
//! shapes are the house wasm ones:
//!
//! - small records are constructible classes with getters (the
//!   `BeaconObservation` precedent);
//! - lists of variable-length byte strings come in as `Uint8Array[]` and go
//!   out through an indexed accessor (`IntroductionsBundle.record(i)`);
//! - lists of fixed-width byte strings go out flat-concatenated
//!   (`introductionTags`, 32 bytes each);
//! - a refusal throws a real `Error` whose message is the core's sentence
//!   and which carries the frozen code as an own property: `reachError`,
//!   `introductionError`, `visibilityError`, `usernameCertificateError`,
//!   `committerDelegationError` (ADR-0060 §6)
//!   (a display-name / username refusal inside an introduction payload
//!   throws the profile's `profileError` shape).
//!
//! Every function is split into a `*_inner` returning a Rust error and the
//! exported wrapper that turns it into a JS value, so the refusal paths are
//! testable on the host (a `js_sys::Error` panics off wasm32).
//!
//! **Reach seeds are secret** (whoever holds one can write to the owner's
//! mailbox): keep them in encrypted storage only, never log them.

use lakko_attest::username_cert::{
    verify_username_binding as rust_verify_username_binding, UsernameCertError,
    UsernameCertificate as RustUsernameCertificate, OPRF_HASH_LEN,
};
use lakko_contacts::committer_delegation::{
    compose_committer_delegation as rust_compose_committer_delegation,
    decode_committer_delegation as rust_decode_committer_delegation,
    CommitterDelegationEnvelope as RustCommitterDelegationEnvelope, CommitterDelegationError,
    ADDED_ACCOUNT_KEY_LEN, COMMITTER_DELEGATION_BLOB_LEN, MAX_COMMITTER_DELEGATIONS,
};
use lakko_contacts::introduction::{
    bundle_is_newer as rust_bundle_is_newer, compose_bundle as rust_compose_bundle,
    compose_introduction as rust_compose_introduction,
    compose_introduction_offer as rust_compose_introduction_offer,
    compose_public_record as rust_compose_public_record,
    decode_introduction_offer as rust_decode_introduction_offer,
    decode_introductions as rust_decode_introductions, fof_tag as rust_fof_tag,
    introduction_tags as rust_introduction_tags, open_introduction as rust_open_introduction,
    open_public_record as rust_open_public_record, IntroductionError,
    IntroductionPayload as RustIntroductionPayload, IntroductionRecord as RustIntroductionRecord,
    IntroductionsBundle as RustIntroductionsBundle, BUNDLE_SIZE_BUCKETS, FOF_TAG_LEN,
    IDENTITY_PK_LEN, MAX_BUNDLE_RECORDS, MAX_INTRODUCTION_TERMS, OPRF_OUTPUT_LEN,
};
use lakko_contacts::profile::{ProfileError, ACCOUNT_ID_LEN};
use lakko_contacts::reach::{
    continuity_reach_seed as rust_continuity_reach_seed, reach_seed as rust_reach_seed,
    reach_verifying_key as rust_reach_verifying_key, ReachClass as RustReachClass, ReachError,
    ReachGrant as RustReachGrant, CONTINUITY_ACCOUNT_ID_LEN, MAX_REACH_SCOPE_ID_LEN,
    REACH_GRANT_LEN, REACH_SEED_LEN,
};
use lakko_contacts::visibility::{
    compose_reach_state as rust_compose_reach_state, compose_visibility as rust_compose_visibility,
    decode_reach_state as rust_decode_reach_state, decode_visibility as rust_decode_visibility,
    merge_reach_state as rust_merge_reach_state, reach_state_is_newer as rust_reach_state_is_newer,
    visibility_is_newer as rust_visibility_is_newer, GroupsForm as RustGroupsForm,
    ReachState as RustReachState, ReachStateEntry as RustReachStateEntry, ScopeId as RustScopeId,
    ScopeKind as RustScopeKind, VisibilityError, VisibilityMode as RustVisibilityMode,
    VisibilitySetting as RustVisibilitySetting, MAX_CHOSEN_SCOPES, MAX_REACH_STATE_ENTRIES,
    MAX_SCOPE_ID_LEN,
};
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::VerifyingKey;
use lakko_ds_proof::{
    check_committer_delegations as rust_check_committer_delegations,
    delegation_for as rust_delegation_for, CommitterDelegationCheckError, Delegation,
    DelegationError,
};
use wasm_bindgen::prelude::*;

use crate::contacts::coded_error;

// ============================================================================
// Errors
// ============================================================================

/// A refusal on its way to JS: the thrown `Error`'s message, the own
/// property it carries and the frozen code in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    /// The own property name (`reachError`, `introductionError`, …).
    pub(crate) property: &'static str,
    /// The frozen code.
    pub(crate) code: String,
    /// The human-readable sentence (never the input).
    pub(crate) message: String,
}

impl Refusal {
    fn new(property: &'static str, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            property,
            code: code.into(),
            message: message.into(),
        }
    }

    pub(crate) fn into_js(self) -> JsValue {
        coded_error(&self.message, self.property, &self.code)
    }
}

impl From<ReachError> for Refusal {
    fn from(e: ReachError) -> Self {
        Self::new("reachError", e.code(), e.to_string())
    }
}

impl From<IntroductionError> for Refusal {
    fn from(e: IntroductionError) -> Self {
        match e {
            // A payload's name and username follow the profile rule, so they
            // throw the profile's shape.
            IntroductionError::DisplayName(d) => {
                let p = ProfileError::DisplayName(d);
                Self::new("profileError", p.code(), p.to_string())
            },
            IntroductionError::Username(u) => {
                let p = ProfileError::Username(u);
                Self::new("profileError", p.code(), p.to_string())
            },
            other => Self::new("introductionError", other.code(), other.to_string()),
        }
    }
}

impl From<lakko_contacts::mailbox::MailboxError> for Refusal {
    fn from(e: lakko_contacts::mailbox::MailboxError) -> Self {
        Self::new("mailboxError", e.code(), e.to_string())
    }
}

/// Decode a 97-byte mailbox binding (layout only), or throw with
/// `property` / `badBinding`.
fn binding_of(
    bytes: &[u8],
    property: &'static str,
) -> Result<lakko_contacts::mailbox::MailboxBinding, Refusal> {
    lakko_contacts::mailbox::MailboxBinding::decode(bytes)
        .map_err(|e| Refusal::new(property, "badBinding", e.to_string()))
}

impl From<VisibilityError> for Refusal {
    fn from(e: VisibilityError) -> Self {
        Self::new("visibilityError", e.code(), e.to_string())
    }
}

/// The frozen code of a username-certificate refusal. `lakko-attest` has no
/// `code()`; `lakko-ffi-uniffi` carries the same table, and each side pins
/// the literals in a test.
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

impl From<UsernameCertError> for Refusal {
    fn from(e: UsernameCertError) -> Self {
        Self::new(
            "usernameCertificateError",
            username_cert_code(&e),
            e.to_string(),
        )
    }
}

fn length_refusal(
    property: &'static str,
    code: &str,
    what: &str,
    want: usize,
    got: usize,
) -> Refusal {
    Refusal::new(
        property,
        code,
        format!("{what} must be {want} bytes, got {got}"),
    )
}

fn seed32(bytes: &[u8], what: &str) -> Result<[u8; REACH_SEED_LEN], Refusal> {
    <[u8; REACH_SEED_LEN]>::try_from(bytes).map_err(|_| {
        length_refusal(
            "reachError",
            "seedLength",
            what,
            REACH_SEED_LEN,
            bytes.len(),
        )
    })
}

fn js<T>(r: Result<T, Refusal>) -> Result<T, JsValue> {
    r.map_err(Refusal::into_js)
}

// ============================================================================
// Reach keys — ADR-0060 §1
// ============================================================================

/// Who a reach key is handed to. The numeric values ARE the frozen wire
/// bytes of `lakko_contacts::reach::ReachClass`.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReachClass {
    /// Every contact. Empty scope.
    Contact = 1,
    /// Whoever holds one invite link. Scope: the capability id.
    Link = 2,
    /// The one party a request of the owner's went to. Scope: a random id.
    Reply = 3,
    /// Members of every group the owner lets find them. Empty scope: one
    /// key per account (ADR-0078 section 9).
    Group = 4,
    /// Every co-member of every group, whatever the setting. Empty scope.
    Member = 5,
    /// Members of every channel the owner lets find them. Empty scope.
    Channel = 6,
    /// Contacts of contacts. Empty scope.
    Fof = 7,
}

impl From<ReachClass> for RustReachClass {
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

impl From<RustReachClass> for ReachClass {
    fn from(c: RustReachClass) -> Self {
        match c {
            RustReachClass::Contact => Self::Contact,
            RustReachClass::Link => Self::Link,
            RustReachClass::Reply => Self::Reply,
            RustReachClass::Group => Self::Group,
            RustReachClass::Member => Self::Member,
            RustReachClass::Channel => Self::Channel,
            RustReachClass::Fof => Self::Fof,
        }
    }
}

/// A reach key handed to someone: class, version, seed, and the owner's
/// 97-byte mailbox binding (ADR-0078). **The seed is secret.** Mirror of
/// `lakko_contacts::reach::ReachGrant` (whose `Debug` redacts the seed).
///
/// **A writer verifies the binding before every write**
/// (`mailboxPkVerified(ownerAccountPk)`): a grant whose binding does not
/// verify must not be written under.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReachGrant {
    inner: RustReachGrant,
}

impl ReachGrant {
    pub(crate) const fn rust(&self) -> &RustReachGrant {
        &self.inner
    }

    pub(crate) const fn from_rust(inner: RustReachGrant) -> Self {
        Self { inner }
    }

    fn new_inner(
        class: ReachClass,
        version: u32,
        seed: &[u8],
        binding: &[u8],
    ) -> Result<Self, Refusal> {
        Ok(Self {
            inner: RustReachGrant {
                class: class.into(),
                version,
                seed: SecretBytes::new(seed32(seed, "reach grant seed")?),
                binding: binding_of(binding, "reachError")?,
            },
        })
    }
}

#[wasm_bindgen]
impl ReachGrant {
    /// Build a grant from its four fields.
    ///
    /// # Errors
    ///
    /// Throws with `reachError` `seedLength` when `seed` is not 32 bytes,
    /// `badBinding` when `binding` is not a well-formed 97-byte binding.
    #[wasm_bindgen(constructor)]
    pub fn new(
        class: ReachClass,
        version: u32,
        seed: &[u8],
        binding: &[u8],
    ) -> Result<ReachGrant, JsValue> {
        js(Self::new_inner(class, version, seed, binding))
    }

    /// The owner's 97-byte mailbox binding, not verified.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn binding(&self) -> Vec<u8> {
        self.inner.binding.encode().to_vec()
    }

    /// The 32-byte mailbox key this grant writes to, after verifying its
    /// binding against `ownerAccountPk`, the owner's 32-byte account key as
    /// the writer holds it pinned (ADR-0078). Call before every write.
    ///
    /// # Errors
    ///
    /// Throws with `mailboxError` `badBinding` / `badKey`, or `keyLength`.
    #[wasm_bindgen(js_name = mailboxPkVerified)]
    pub fn mailbox_pk_verified(&self, owner_account_pk: &[u8]) -> Result<Vec<u8>, JsValue> {
        js(self.mailbox_pk_verified_inner(owner_account_pk))
    }

    /// Who the key is for.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn class(&self) -> ReachClass {
        self.inner.class.into()
    }

    /// The key version.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn version(&self) -> u32 {
        self.inner.version
    }

    /// The 32-byte seed. Secret.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn seed(&self) -> Vec<u8> {
        self.inner.seed.as_bytes().to_vec()
    }

    /// The 32-byte public key the holder signs mailbox writes under.
    #[wasm_bindgen(js_name = verifyingKey)]
    #[must_use]
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_vec()
    }
}

impl ReachGrant {
    fn mailbox_pk_verified_inner(&self, owner_account_pk: &[u8]) -> Result<Vec<u8>, Refusal> {
        let account = <[u8; REACH_SEED_LEN]>::try_from(owner_account_pk).map_err(|_| {
            length_refusal(
                "mailboxError",
                "keyLength",
                "account key",
                REACH_SEED_LEN,
                owner_account_pk.len(),
            )
        })?;
        Ok(self.inner.mailbox_pk_verified(&account)?.to_vec())
    }
}

fn reach_seed_inner(
    account_seed: &[u8],
    class: ReachClass,
    scope_id: &[u8],
    version: u32,
) -> Result<Vec<u8>, Refusal> {
    let account = seed32(account_seed, "account seed")?;
    Ok(rust_reach_seed(&account, class.into(), scope_id, version)?
        .as_bytes()
        .to_vec())
}

/// Derive the 32-byte reach seed for `(class, scopeId, version)` from the
/// 32-byte account seed. Every device of the account derives the same seed.
///
/// # Errors
///
/// Throws with `reachError` `seedLength`, `scopeIdTooLong`,
/// `scopeIdNotEmpty` or `scopeIdEmpty`.
#[wasm_bindgen(js_name = reachSeed)]
pub fn reach_seed(
    account_seed: &[u8],
    class: ReachClass,
    scope_id: &[u8],
    version: u32,
) -> Result<Vec<u8>, JsValue> {
    js(reach_seed_inner(account_seed, class, scope_id, version))
}

fn continuity_reach_seed_inner(account_id: &[u8], key_generation: u64) -> Result<Vec<u8>, Refusal> {
    let id = <[u8; CONTINUITY_ACCOUNT_ID_LEN]>::try_from(account_id).map_err(|_| {
        length_refusal(
            "reachError",
            "accountIdLength",
            "account id",
            CONTINUITY_ACCOUNT_ID_LEN,
            account_id.len(),
        )
    })?;
    Ok(rust_continuity_reach_seed(&id, key_generation)
        .as_bytes()
        .to_vec())
}

/// The 32-byte **continuity reach seed** of an account at a key generation
/// (ADR-0073 R.4): `BLAKE3-derive-key("lakko/reach/continuity/v1",
/// accountId(16) || keyGeneration(8, BE))`.
///
/// After an account key reset the resetting device registers
/// `reachVerifyingKey(seed)` on its new mailbox (with an expiry) and a peer
/// that accepted the reset signs its Welcome with
/// `DsProofSigner.forReach(seed)`. `keyGeneration` (a `bigint`) is the
/// generation the reset moved to, as the verified key-reset attestation
/// states it. No input is secret: it is a reach key, never a credential.
///
/// # Errors
///
/// Throws with `reachError` `accountIdLength`.
#[wasm_bindgen(js_name = continuityReachSeed)]
pub fn continuity_reach_seed(account_id: &[u8], key_generation: u64) -> Result<Vec<u8>, JsValue> {
    js(continuity_reach_seed_inner(account_id, key_generation))
}

fn derive_reach_grant_inner(
    account_seed: &[u8],
    class: ReachClass,
    scope_id: &[u8],
    version: u32,
) -> Result<ReachGrant, Refusal> {
    let account = seed32(account_seed, "account seed")?;
    Ok(ReachGrant {
        inner: RustReachGrant::derive(&account, class.into(), scope_id, version)?,
    })
}

/// `reachSeed` wrapped as a grant, ready to hand out.
///
/// # Errors
///
/// Exactly `reachSeed`'s.
#[wasm_bindgen(js_name = deriveReachGrant)]
pub fn derive_reach_grant(
    account_seed: &[u8],
    class: ReachClass,
    scope_id: &[u8],
    version: u32,
) -> Result<ReachGrant, JsValue> {
    js(derive_reach_grant_inner(
        account_seed,
        class,
        scope_id,
        version,
    ))
}

/// The 32-byte Ed25519 public key of a reach seed.
///
/// # Errors
///
/// Throws with `reachError` `seedLength`.
#[wasm_bindgen(js_name = reachVerifyingKey)]
pub fn reach_verifying_key(seed: &[u8]) -> Result<Vec<u8>, JsValue> {
    js(seed32(seed, "reach seed").map(|s| rust_reach_verifying_key(&s).to_vec()))
}

/// Encode a grant as its 134-byte wire (`class || version || seed ||
/// binding`). Holds the seed: seal it before it leaves the device.
#[wasm_bindgen(js_name = encodeReachGrant)]
#[must_use]
pub fn encode_reach_grant(grant: &ReachGrant) -> Vec<u8> {
    grant.inner.encode().to_vec()
}

fn decode_reach_grant_inner(bytes: &[u8]) -> Result<ReachGrant, Refusal> {
    Ok(ReachGrant {
        inner: RustReachGrant::decode(bytes)?,
    })
}

/// Decode exactly `reachGrantLen()` bytes, fail-closed.
///
/// # Errors
///
/// Throws with `reachError` `badLength`, `unknownClass` or `badBinding`. The
/// binding's signature is not checked (`mailboxPkVerified` does).
#[wasm_bindgen(js_name = decodeReachGrant)]
pub fn decode_reach_grant(bytes: &[u8]) -> Result<ReachGrant, JsValue> {
    js(decode_reach_grant_inner(bytes))
}

/// Length of an account seed, a reach seed and a reach public key: 32.
#[wasm_bindgen(js_name = reachSeedLen)]
#[must_use]
pub fn reach_seed_len() -> usize {
    REACH_SEED_LEN
}

/// Wire length of a reach grant: 134.
#[wasm_bindgen(js_name = reachGrantLen)]
#[must_use]
pub fn reach_grant_len() -> usize {
    REACH_GRANT_LEN
}

/// Longest scope id a reach key is derived over: 64.
#[wasm_bindgen(js_name = maxReachScopeIdLen)]
#[must_use]
pub fn max_reach_scope_id_len() -> usize {
    MAX_REACH_SCOPE_ID_LEN
}

// ============================================================================
// Introductions — ADR-0060 §4
// ============================================================================

/// What an introduction record reveals to a searcher who opens it. The
/// introduced person's claim: verify the username with
/// `verifyUsernameBinding` and run the DS reach check before showing it.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntroductionPayload {
    inner: RustIntroductionPayload,
}

impl IntroductionPayload {
    fn new_inner(
        identity_pk: &[u8],
        account_id: &[u8],
        display_name: Option<String>,
        username: Option<String>,
        username_certificate: Option<Vec<u8>>,
        mailbox_binding: lakko_contacts::mailbox::MailboxBinding,
        fof: Option<&ReachGrant>,
    ) -> Result<Self, Refusal> {
        let identity_pk = <[u8; IDENTITY_PK_LEN]>::try_from(identity_pk).map_err(|_| {
            length_refusal(
                "introductionError",
                "identityPkLength",
                "identity key",
                IDENTITY_PK_LEN,
                identity_pk.len(),
            )
        })?;
        let account_id = <[u8; ACCOUNT_ID_LEN]>::try_from(account_id).map_err(|_| {
            length_refusal(
                "introductionError",
                "accountIdLength",
                "account id",
                ACCOUNT_ID_LEN,
                account_id.len(),
            )
        })?;
        Ok(Self {
            inner: RustIntroductionPayload {
                identity_pk,
                account_id,
                display_name,
                username,
                username_certificate,
                mailbox_binding,
                fof: fof.map(|g| g.inner.clone()),
            },
        })
    }
}

#[wasm_bindgen]
impl IntroductionPayload {
    /// Build a contacts-of-contacts payload carrying its `Fof` grant; the
    /// payload's mailbox binding (ADR-0078) is the grant's. The field rules
    /// (name, username, certificate, a `Fof` grant, a binding that verifies
    /// under `identityPk`) are checked by `composeIntroduction`.
    ///
    /// # Errors
    ///
    /// Throws with `introductionError` `identityPkLength` or
    /// `accountIdLength`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        identity_pk: &[u8],
        account_id: &[u8],
        display_name: Option<String>,
        username: Option<String>,
        username_certificate: Option<Vec<u8>>,
        fof: &ReachGrant,
    ) -> Result<IntroductionPayload, JsValue> {
        js(Self::new_inner(
            identity_pk,
            account_id,
            display_name,
            username,
            username_certificate,
            fof.inner.binding,
            Some(fof),
        ))
    }

    /// Build the payload of an Everyone public record (ADR-0060 section 5):
    /// the same fields with no grant, and the owner's 97-byte mailbox binding
    /// (`signMailboxBinding`, ADR-0078) so a finder knows where to write.
    /// Pass it to `composePublicRecord`.
    ///
    /// # Errors
    ///
    /// Throws with `introductionError` `identityPkLength`,
    /// `accountIdLength` or `badBinding`.
    #[wasm_bindgen(js_name = forPublicRecord)]
    pub fn for_public_record(
        identity_pk: &[u8],
        account_id: &[u8],
        display_name: Option<String>,
        username: Option<String>,
        username_certificate: Option<Vec<u8>>,
        mailbox_binding: &[u8],
    ) -> Result<IntroductionPayload, JsValue> {
        js(
            binding_of(mailbox_binding, "introductionError").and_then(|binding| {
                Self::new_inner(
                    identity_pk,
                    account_id,
                    display_name,
                    username,
                    username_certificate,
                    binding,
                    None,
                )
            }),
        )
    }

    /// The owner's 97-byte mailbox binding (ADR-0078), verified against
    /// `identityPk` when the payload was composed or opened.
    #[wasm_bindgen(getter, js_name = mailboxBinding)]
    #[must_use]
    pub fn mailbox_binding(&self) -> Vec<u8> {
        self.inner.mailbox_binding.encode().to_vec()
    }

    /// The 32-byte identity (account) public key.
    #[wasm_bindgen(getter, js_name = identityPk)]
    #[must_use]
    pub fn identity_pk(&self) -> Vec<u8> {
        self.inner.identity_pk.to_vec()
    }

    /// The 16-byte account id.
    #[wasm_bindgen(getter, js_name = accountId)]
    #[must_use]
    pub fn account_id(&self) -> Vec<u8> {
        self.inner.account_id.to_vec()
    }

    /// The display name, or `undefined`.
    #[wasm_bindgen(getter, js_name = displayName)]
    #[must_use]
    pub fn display_name(&self) -> Option<String> {
        self.inner.display_name.clone()
    }

    /// The canonical username, or `undefined`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn username(&self) -> Option<String> {
        self.inner.username.clone()
    }

    /// The 153-byte username certificate, or `undefined`.
    #[wasm_bindgen(getter, js_name = usernameCertificate)]
    #[must_use]
    pub fn username_certificate(&self) -> Option<Vec<u8>> {
        self.inner.username_certificate.clone()
    }

    /// The `Fof` reach grant (secret); `undefined` in a public record.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn fof(&self) -> Option<ReachGrant> {
        self.inner
            .fof
            .as_ref()
            .map(|g| ReachGrant { inner: g.clone() })
    }
}

fn oprf64(bytes: &[u8]) -> Result<[u8; OPRF_OUTPUT_LEN], Refusal> {
    <[u8; OPRF_OUTPUT_LEN]>::try_from(bytes).map_err(|_| {
        length_refusal(
            "introductionError",
            "oprfLength",
            "an OPRF output",
            OPRF_OUTPUT_LEN,
            bytes.len(),
        )
    })
}

fn compose_introduction_inner(
    payload: &IntroductionPayload,
    term_oprfs: &[Vec<u8>],
    issued_at_ms: u64,
) -> Result<Vec<u8>, Refusal> {
    let terms = term_oprfs
        .iter()
        .map(|t| oprf64(t))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rust_compose_introduction(
        &payload.inner,
        &terms,
        issued_at_ms,
    )?)
}

/// Build one introduction record findable by each search term, supplied as
/// its 64-byte server OPRF output (`Uint8Array[]`, order kept). Returns the
/// record wire.
///
/// # Errors
///
/// Throws with `introductionError` (`oprfLength`, `noTerms`,
/// `tooManyTerms`, `duplicateTag`, `certificate.length`, `grant.notFof`, …)
/// or `profileError` for the name / username.
#[wasm_bindgen(js_name = composeIntroduction)]
pub fn compose_introduction(
    payload: &IntroductionPayload,
    term_oprfs: Vec<js_sys::Uint8Array>,
    issued_at_ms: u64,
) -> Result<Vec<u8>, JsValue> {
    let owned: Vec<Vec<u8>> = term_oprfs.into_iter().map(|a| a.to_vec()).collect();
    js(compose_introduction_inner(payload, &owned, issued_at_ms))
}

fn introduction_tags_inner(record: &[u8]) -> Result<Vec<u8>, Refusal> {
    Ok(rust_introduction_tags(record)?.concat())
}

/// The tags of a record wire, flat-concatenated (`fofTagLen()` = 32 bytes
/// each, in entry order), to match against `fofTag` without opening.
///
/// # Errors
///
/// Throws with `introductionError` for a malformed record.
#[wasm_bindgen(js_name = introductionTags)]
pub fn introduction_tags(record: &[u8]) -> Result<Vec<u8>, JsValue> {
    js(introduction_tags_inner(record))
}

/// The tag a record entry for one 64-byte OPRF output carries.
///
/// # Errors
///
/// Throws with `introductionError` `oprfLength`.
#[wasm_bindgen(js_name = fofTag)]
pub fn fof_tag(oprf_output: &[u8]) -> Result<Vec<u8>, JsValue> {
    js(oprf64(oprf_output).map(|o| rust_fof_tag(&o).to_vec()))
}

fn open_introduction_inner(
    record: &[u8],
    oprf_output: &[u8],
) -> Result<Option<IntroductionPayload>, Refusal> {
    Ok(rust_open_introduction(record, &oprf64(oprf_output)?)?
        .map(|inner| IntroductionPayload { inner }))
}

/// Open a record wire with one 64-byte OPRF output; `undefined` when no
/// entry matches (not this person, or a padding dummy). A payload returned
/// always carries its `fof` grant.
///
/// # Errors
///
/// Throws with `introductionError` `open` when a tag matched but the record
/// failed to authenticate (a forgery, never a miss), `grant.missing` when
/// it opened without a grant, or for a malformed record.
#[wasm_bindgen(js_name = openIntroduction)]
pub fn open_introduction(
    record: &[u8],
    oprf_output: &[u8],
) -> Result<Option<IntroductionPayload>, JsValue> {
    js(open_introduction_inner(record, oprf_output))
}

fn compose_public_record_inner(
    payload: &IntroductionPayload,
    term_oprfs: &[Vec<u8>],
    issued_at_ms: u64,
) -> Result<Vec<u8>, Refusal> {
    let terms = term_oprfs
        .iter()
        .map(|t| oprf64(t))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rust_compose_public_record(
        &payload.inner,
        &terms,
        issued_at_ms,
    )?)
}

/// Build the public record a person in Everyone publishes with
/// `PUT /discovery/visibility` (ADR-0060 section 5): the introduction record
/// shape, findable by the username's OPRF output and every name term's,
/// with NO grant (build the payload with `IntroductionPayload.forPublicRecord`).
///
/// # Errors
///
/// Throws with `introductionError` `grant.unexpected` when the payload
/// carries a grant, and every error of `composeIntroduction`.
#[wasm_bindgen(js_name = composePublicRecord)]
pub fn compose_public_record(
    payload: &IntroductionPayload,
    term_oprfs: Vec<js_sys::Uint8Array>,
    issued_at_ms: u64,
) -> Result<Vec<u8>, JsValue> {
    let owned: Vec<Vec<u8>> = term_oprfs.into_iter().map(|a| a.to_vec()).collect();
    js(compose_public_record_inner(payload, &owned, issued_at_ms))
}

fn open_public_record_inner(
    record: &[u8],
    oprf_output: &[u8],
) -> Result<Option<IntroductionPayload>, Refusal> {
    Ok(rust_open_public_record(record, &oprf64(oprf_output)?)?
        .map(|inner| IntroductionPayload { inner }))
}

/// Open a public record served by `/discovery/lookup` or
/// `/discovery/name/lookup` with the OPRF output of the searched username or
/// name term; `undefined` when no entry matches. A payload returned never
/// carries a grant (the person's mailbox is open).
///
/// # Errors
///
/// Throws with `introductionError` `grant.unexpected` when it opened with a
/// grant, and every error of `openIntroduction`.
#[wasm_bindgen(js_name = openPublicRecord)]
pub fn open_public_record(
    record: &[u8],
    oprf_output: &[u8],
) -> Result<Option<IntroductionPayload>, JsValue> {
    js(open_public_record_inner(record, oprf_output))
}

/// Wrap one record as an introduction offer (owner to a contact, in their
/// 1:1 conversation).
///
/// # Errors
///
/// Throws with `introductionError` for a malformed record.
#[wasm_bindgen(js_name = composeIntroductionOffer)]
pub fn compose_introduction_offer(record: &[u8]) -> Result<Vec<u8>, JsValue> {
    js(rust_compose_introduction_offer(record).map_err(Refusal::from))
}

fn decode_introduction_offer_inner(plaintext: &[u8]) -> Result<Vec<u8>, Refusal> {
    Ok(rust_decode_introduction_offer(plaintext)?.encode()?)
}

/// Strictly decode an introduction offer, returning the record wire. Accept
/// one only from the other participant of a 1:1 conversation.
///
/// # Errors
///
/// Throws with `introductionError` (`wrongDomain`, `unknownVersion`, a
/// record error).
#[wasm_bindgen(js_name = decodeIntroductionOffer)]
pub fn decode_introduction_offer(plaintext: &[u8]) -> Result<Vec<u8>, JsValue> {
    js(decode_introduction_offer_inner(plaintext))
}

/// A contact's decoded bundle of introduction records, as record wires.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntroductionsBundle {
    updated_at_ms: u64,
    records: Vec<Vec<u8>>,
}

impl IntroductionsBundle {
    pub(crate) fn from_rust(b: &RustIntroductionsBundle) -> Result<Self, Refusal> {
        Ok(Self {
            updated_at_ms: b.updated_at_ms,
            records: b
                .records
                .iter()
                .map(RustIntroductionRecord::encode)
                .collect::<Result<_, _>>()?,
        })
    }
}

#[wasm_bindgen]
impl IntroductionsBundle {
    /// Sender-clock UNIX ms, a `bigint`; orders bundles from one contact via
    /// `bundleIsNewer`.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    #[must_use]
    pub fn updated_at_ms(&self) -> u64 {
        self.updated_at_ms
    }

    /// How many records, padding included.
    #[wasm_bindgen(js_name = recordCount)]
    #[must_use]
    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    /// The record wire at `index`, or `undefined` past the end.
    #[must_use]
    pub fn record(&self, index: usize) -> Option<Vec<u8>> {
        self.records.get(index).cloned()
    }
}

fn compose_introductions_bundle_inner(
    records: &[Vec<u8>],
    updated_at_ms: u64,
) -> Result<Vec<u8>, Refusal> {
    let decoded = records
        .iter()
        .map(|r| RustIntroductionRecord::decode(r))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rust_compose_bundle(&decoded, updated_at_ms)?)
}

/// Build a bundle from the real record wires this device holds
/// (`Uint8Array[]`): padded to the next size bucket, shuffled, encoded. Sent
/// to each contact in their 1:1 conversation; never re-relayed.
///
/// # Errors
///
/// Throws with `introductionError` `tooManyRecords` or a record error.
#[wasm_bindgen(js_name = composeIntroductionsBundle)]
pub fn compose_introductions_bundle(
    records: Vec<js_sys::Uint8Array>,
    updated_at_ms: u64,
) -> Result<Vec<u8>, JsValue> {
    let owned: Vec<Vec<u8>> = records.into_iter().map(|a| a.to_vec()).collect();
    js(compose_introductions_bundle_inner(&owned, updated_at_ms))
}

fn decode_introductions_bundle_inner(plaintext: &[u8]) -> Result<IntroductionsBundle, Refusal> {
    IntroductionsBundle::from_rust(&rust_decode_introductions(plaintext)?)
}

/// Strictly decode an introductions bundle.
///
/// # Errors
///
/// Throws with `introductionError` (`wrongDomain`, `badRecordCount`,
/// `recordLength`, a record error).
#[wasm_bindgen(js_name = decodeIntroductionsBundle)]
pub fn decode_introductions_bundle(plaintext: &[u8]) -> Result<IntroductionsBundle, JsValue> {
    js(decode_introductions_bundle_inner(plaintext))
}

/// `true` when a bundle stamped `incomingUpdatedAtMs` must replace the stored
/// one from the same contact. Strictly greater wins.
#[wasm_bindgen(js_name = bundleIsNewer)]
#[must_use]
pub fn bundle_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_bundle_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Most search terms one record carries: 65.
#[wasm_bindgen(js_name = maxIntroductionTerms)]
#[must_use]
pub fn max_introduction_terms() -> usize {
    MAX_INTRODUCTION_TERMS
}

/// Most records one bundle carries, padding included: 1024.
#[wasm_bindgen(js_name = maxBundleRecords)]
#[must_use]
pub fn max_bundle_records() -> usize {
    MAX_BUNDLE_RECORDS
}

/// The record counts a bundle may carry.
#[wasm_bindgen(js_name = bundleSizeBuckets)]
#[must_use]
pub fn bundle_size_buckets() -> Vec<u32> {
    BUNDLE_SIZE_BUCKETS
        .iter()
        .map(|&b| u32::try_from(b).unwrap_or(u32::MAX))
        .collect()
}

/// Length of a server OPRF output: 64.
#[wasm_bindgen(js_name = introductionOprfOutputLen)]
#[must_use]
pub fn introduction_oprf_output_len() -> usize {
    OPRF_OUTPUT_LEN
}

/// Length of a record tag: 32.
#[wasm_bindgen(js_name = fofTagLen)]
#[must_use]
pub fn fof_tag_len() -> usize {
    FOF_TAG_LEN
}

// ============================================================================
// The setting — ADR-0060 §7
// ============================================================================

/// The three positions. Numeric values are the frozen wire bytes.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibilityMode {
    /// No one finds the person by username or name.
    Nobody = 0,
    /// The parts that are on.
    Some = 1,
    /// Anyone finds the person.
    Everyone = 2,
}

impl From<VisibilityMode> for RustVisibilityMode {
    fn from(m: VisibilityMode) -> Self {
        match m {
            VisibilityMode::Nobody => Self::Nobody,
            VisibilityMode::Some => Self::Some,
            VisibilityMode::Everyone => Self::Everyone,
        }
    }
}

impl From<RustVisibilityMode> for VisibilityMode {
    fn from(m: RustVisibilityMode) -> Self {
        match m {
            RustVisibilityMode::Nobody => Self::Nobody,
            RustVisibilityMode::Some => Self::Some,
            RustVisibilityMode::Everyone => Self::Everyone,
        }
    }
}

/// All groups and channels, or only the chosen ones. Numeric values are the
/// frozen wire bytes (0 on the wire is "off", which is `undefined` here).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupsForm {
    /// Every group and channel the person is in.
    All = 1,
    /// Only the ones in the chosen list.
    Chosen = 2,
}

impl From<GroupsForm> for RustGroupsForm {
    fn from(f: GroupsForm) -> Self {
        match f {
            GroupsForm::All => Self::All,
            GroupsForm::Chosen => Self::Chosen,
        }
    }
}

impl From<RustGroupsForm> for GroupsForm {
    fn from(f: RustGroupsForm) -> Self {
        match f {
            RustGroupsForm::All => Self::All,
            RustGroupsForm::Chosen => Self::Chosen,
        }
    }
}

/// Group or channel. Numeric values are the frozen wire bytes.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// A group (MLS group id).
    Group = 1,
    /// A channel (channel id).
    Channel = 2,
}

/// One group or channel.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeId {
    kind: ScopeKind,
    id: Vec<u8>,
}

#[wasm_bindgen]
impl ScopeId {
    /// Build one. The id length (1 to 64 bytes) is checked where it is used.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(kind: ScopeKind, id: Vec<u8>) -> ScopeId {
        Self { kind, id }
    }

    /// Group or channel.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> ScopeKind {
        self.kind
    }

    /// The id.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn id(&self) -> Vec<u8> {
        self.id.clone()
    }
}

impl From<&ScopeId> for RustScopeId {
    fn from(s: &ScopeId) -> Self {
        Self {
            kind: match s.kind {
                ScopeKind::Group => RustScopeKind::Group,
                ScopeKind::Channel => RustScopeKind::Channel,
            },
            id: s.id.clone(),
        }
    }
}

impl From<RustScopeId> for ScopeId {
    fn from(s: RustScopeId) -> Self {
        Self {
            kind: match s.kind {
                RustScopeKind::Group => ScopeKind::Group,
                RustScopeKind::Channel => ScopeKind::Channel,
            },
            id: s.id,
        }
    }
}

/// The "who can find me" setting. Edit it only through the Settings
/// transitions (`chooseVisibilityMode`, …), stamp it with
/// `withUpdatedAtMs`, then compose the envelope.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibilitySetting {
    inner: RustVisibilitySetting,
}

impl VisibilitySetting {
    pub(crate) const fn rust(&self) -> &RustVisibilitySetting {
        &self.inner
    }

    pub(crate) const fn from_rust(inner: RustVisibilitySetting) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen]
impl VisibilitySetting {
    /// Build a setting from all of its fields (as stored). Validity is
    /// checked by `composeVisibilityEnvelope`.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(
        mode: VisibilityMode,
        contacts_of_contacts: bool,
        groups: Option<GroupsForm>,
        chosen: Vec<ScopeId>,
        last_contacts_of_contacts: bool,
        last_groups: Option<GroupsForm>,
        updated_at_ms: u64,
    ) -> VisibilitySetting {
        Self {
            inner: RustVisibilitySetting {
                mode: mode.into(),
                contacts_of_contacts,
                groups: groups.map(Into::into),
                chosen: chosen.iter().map(Into::into).collect(),
                last_contacts_of_contacts,
                last_groups: last_groups.map(Into::into),
                updated_at_ms,
            },
        }
    }

    /// The position.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn mode(&self) -> VisibilityMode {
        self.inner.mode.into()
    }

    /// Part 2.1: contacts of my contacts.
    #[wasm_bindgen(getter, js_name = contactsOfContacts)]
    #[must_use]
    pub fn contacts_of_contacts(&self) -> bool {
        self.inner.contacts_of_contacts
    }

    /// Part 2.2 and its form; `undefined` when off.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn groups(&self) -> Option<GroupsForm> {
        self.inner.groups.map(Into::into)
    }

    /// The chosen groups and channels (kept whatever the form).
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn chosen(&self) -> Vec<ScopeId> {
        self.inner.chosen.iter().cloned().map(Into::into).collect()
    }

    /// Memory: part 2.1 when the person last left Some people.
    #[wasm_bindgen(getter, js_name = lastContactsOfContacts)]
    #[must_use]
    pub fn last_contacts_of_contacts(&self) -> bool {
        self.inner.last_contacts_of_contacts
    }

    /// Memory: part 2.2's form when the person last left Some people.
    #[wasm_bindgen(getter, js_name = lastGroups)]
    #[must_use]
    pub fn last_groups(&self) -> Option<GroupsForm> {
        self.inner.last_groups.map(Into::into)
    }

    /// Setter-clock UNIX ms, a `bigint`.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    #[must_use]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }

    /// The same setting stamped `updatedAtMs` (the transitions never stamp).
    #[wasm_bindgen(js_name = withUpdatedAtMs)]
    #[must_use]
    pub fn with_updated_at_ms(&self, updated_at_ms: u64) -> VisibilitySetting {
        let mut inner = self.inner.clone();
        inner.updated_at_ms = updated_at_ms;
        Self { inner }
    }
}

/// The default for a new account: Some people with contacts of contacts and
/// all groups and channels on; `updatedAtMs` 0.
#[wasm_bindgen(js_name = defaultVisibilitySetting)]
#[must_use]
pub fn default_visibility_setting() -> VisibilitySetting {
    VisibilitySetting {
        inner: RustVisibilitySetting::default_for_new_account(),
    }
}

/// Compose the setting envelope, sent over the owner's DeviceGroup.
///
/// # Errors
///
/// Throws with `visibilityError` (`someWithNoPart`, `partsOutsideSome`,
/// `tooManyChosen`, `duplicateScope`, `scopeIdLength`).
#[wasm_bindgen(js_name = composeVisibilityEnvelope)]
pub fn compose_visibility_envelope(setting: &VisibilitySetting) -> Result<Vec<u8>, JsValue> {
    js(rust_compose_visibility(&setting.inner).map_err(Refusal::from))
}

fn decode_visibility_inner(plaintext: &[u8]) -> Result<VisibilitySetting, Refusal> {
    Ok(VisibilitySetting {
        inner: rust_decode_visibility(plaintext)?,
    })
}

/// Strictly decode a setting envelope. Accept one only on the owner's own
/// DeviceGroup, and keep it only when `visibilityIsNewer`.
///
/// # Errors
///
/// Throws with `visibilityError`.
#[wasm_bindgen(js_name = decodeVisibilityEnvelope)]
pub fn decode_visibility_envelope(plaintext: &[u8]) -> Result<VisibilitySetting, JsValue> {
    js(decode_visibility_inner(plaintext))
}

/// `true` when a setting stamped `incomingUpdatedAtMs` must replace the
/// stored one. Strictly greater wins.
#[wasm_bindgen(js_name = visibilityIsNewer)]
#[must_use]
pub fn visibility_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_visibility_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Whether the members of `scope` can find the person. A permission, not a
/// delivery decision.
#[wasm_bindgen(js_name = allowsScope)]
#[must_use]
pub fn allows_scope(setting: &VisibilitySetting, scope: &ScopeId) -> bool {
    setting.inner.allows_scope(&scope.into())
}

/// Whether contacts of contacts can find the person.
#[wasm_bindgen(js_name = allowsContactsOfContacts)]
#[must_use]
pub fn allows_contacts_of_contacts(setting: &VisibilitySetting) -> bool {
    setting.inner.allows_contacts_of_contacts()
}

/// Most groups and channels the chosen list holds: 1024.
#[wasm_bindgen(js_name = maxChosenScopes)]
#[must_use]
pub fn max_chosen_scopes() -> usize {
    MAX_CHOSEN_SCOPES
}

/// Longest group or channel id in the chosen list: 64.
#[wasm_bindgen(js_name = maxVisibilityScopeIdLen)]
#[must_use]
pub fn max_visibility_scope_id_len() -> usize {
    MAX_SCOPE_ID_LEN
}

// ============================================================================
// The reach state
// ============================================================================

/// One reach key's version counter.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReachStateEntry {
    class: ReachClass,
    scope_id: Vec<u8>,
    version: u32,
}

#[wasm_bindgen]
impl ReachStateEntry {
    /// Build one. The scope's shape is checked by
    /// `composeReachStateEnvelope`.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(class: ReachClass, scope_id: Vec<u8>, version: u32) -> ReachStateEntry {
        Self {
            class,
            scope_id,
            version,
        }
    }

    /// The class.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn class(&self) -> ReachClass {
        self.class
    }

    /// The scope (empty for every class but `Link` / `Reply`).
    #[wasm_bindgen(getter, js_name = scopeId)]
    #[must_use]
    pub fn scope_id(&self) -> Vec<u8> {
        self.scope_id.clone()
    }

    /// The current key version.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }
}

/// Every reach key version counter the owner's devices share.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReachState {
    inner: RustReachState,
}

impl ReachState {
    pub(crate) const fn from_rust(inner: RustReachState) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen]
impl ReachState {
    /// Build a state. Validity is checked by `composeReachStateEnvelope`.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(entries: Vec<ReachStateEntry>, updated_at_ms: u64) -> ReachState {
        Self {
            inner: RustReachState {
                entries: entries
                    .into_iter()
                    .map(|e| RustReachStateEntry {
                        class: e.class.into(),
                        scope_id: e.scope_id,
                        version: e.version,
                    })
                    .collect(),
                updated_at_ms,
            },
        }
    }

    /// The entries.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn entries(&self) -> Vec<ReachStateEntry> {
        self.inner
            .entries
            .iter()
            .map(|e| ReachStateEntry {
                class: e.class.into(),
                scope_id: e.scope_id.clone(),
                version: e.version,
            })
            .collect()
    }

    /// Setter-clock UNIX ms, a `bigint`.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    #[must_use]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }
}

/// Compose the reach-state envelope, sent over the owner's DeviceGroup.
///
/// # Errors
///
/// Throws with `visibilityError` (`tooManyEntries`, `duplicateEntry`,
/// `reach.*`).
#[wasm_bindgen(js_name = composeReachStateEnvelope)]
pub fn compose_reach_state_envelope(state: &ReachState) -> Result<Vec<u8>, JsValue> {
    js(rust_compose_reach_state(&state.inner).map_err(Refusal::from))
}

fn decode_reach_state_inner(plaintext: &[u8]) -> Result<ReachState, Refusal> {
    Ok(ReachState {
        inner: rust_decode_reach_state(plaintext)?,
    })
}

/// Strictly decode a reach-state envelope, ordered by `reachStateIsNewer`.
///
/// # Errors
///
/// Throws with `visibilityError`.
#[wasm_bindgen(js_name = decodeReachStateEnvelope)]
pub fn decode_reach_state_envelope(plaintext: &[u8]) -> Result<ReachState, JsValue> {
    js(decode_reach_state_inner(plaintext))
}

/// `true` when a reach state stamped `incomingUpdatedAtMs` must replace the
/// stored one. Strictly greater wins.
#[wasm_bindgen(js_name = reachStateIsNewer)]
#[must_use]
pub fn reach_state_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_reach_state_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// What `mergeReachState` tells the receiving device to do.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct ReachStateMerge {
    state: ReachState,
    store: bool,
    send: bool,
}

#[wasm_bindgen]
impl ReachStateMerge {
    /// The state this device holds after the merge.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn state(&self) -> ReachState {
        self.state.clone()
    }

    /// Persist `state` and re-derive the keys: it differs from what was
    /// stored.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn store(&self) -> bool {
        self.store
    }

    /// Send `state` to the account's other devices, as it is (never
    /// re-stamped).
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn send(&self) -> bool {
        self.send
    }
}

/// Fold a reach state received from another of the owner's devices into the
/// stored one (ADR-0060 section 7): counters by maximum per slot, never a
/// whole-state replace, and the core decides whether to persist and whether
/// to send the result on. Every client uses this rule; none re-implements it.
///
/// # Errors
///
/// Throws with `visibilityError` when the merged state breaks a bound; keep
/// what was stored.
#[wasm_bindgen(js_name = mergeReachState)]
pub fn merge_reach_state(
    stored: &ReachState,
    incoming: &ReachState,
    now_ms: u64,
) -> Result<ReachStateMerge, JsValue> {
    js(
        rust_merge_reach_state(&stored.inner, &incoming.inner, now_ms)
            .map(|m| ReachStateMerge {
                state: ReachState::from_rust(m.state),
                store: m.store,
                send: m.send,
            })
            .map_err(Refusal::from),
    )
}

/// The version of `(class, scopeId)` in `state`, or `undefined`.
#[wasm_bindgen(js_name = reachStateVersionOf)]
#[must_use]
pub fn reach_state_version_of(
    state: &ReachState,
    class: ReachClass,
    scope_id: &[u8],
) -> Option<u32> {
    state.inner.version_of(class.into(), scope_id)
}

/// Most entries a reach state holds: 4096.
#[wasm_bindgen(js_name = maxReachStateEntries)]
#[must_use]
pub fn max_reach_state_entries() -> usize {
    MAX_REACH_STATE_ENTRIES
}

// ============================================================================
// Committer delegations — ADR-0060 §6
// ============================================================================

impl From<CommitterDelegationError> for Refusal {
    fn from(e: CommitterDelegationError) -> Self {
        Self::new("committerDelegationError", e.code(), e.to_string())
    }
}

impl From<CommitterDelegationCheckError> for Refusal {
    fn from(e: CommitterDelegationCheckError) -> Self {
        Self::new("committerDelegationError", e.code(), e.to_string())
    }
}

fn delegation_refusal(e: &DelegationError) -> Refusal {
    let code = match e {
        DelegationError::UnknownVersion => "delegationUnknownVersion",
        _ => "delegationMalformed",
    };
    Refusal::new("committerDelegationError", code, e.to_string())
}

/// A proposer's committer delegations (ADR-0060 §6). Mirror of
/// `lakko_contacts::committer_delegation::CommitterDelegationEnvelope`.
///
/// Under ADR-0045 the member who proposes an `Add` may not be the one who
/// commits it, and the added person's mailbox is closed. So the proposer
/// posts, in the group beside its proposal, one delegation per committer
/// candidate (the owner and each admin), each signed with the proposer's
/// reach key for the added person's mailbox (`signDelegationBlob`) and naming
/// that candidate's account signature key as delegate. A delegation lives at
/// most 24 hours: **re-send a fresh envelope while the proposal is pending**,
/// and whenever the candidate set changes.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitterDelegationEnvelope {
    inner: RustCommitterDelegationEnvelope,
}

impl CommitterDelegationEnvelope {
    pub(crate) const fn from_rust(inner: RustCommitterDelegationEnvelope) -> Self {
        Self { inner }
    }

    fn new_inner(
        added_account_key: &[u8],
        added_mailbox_binding: &[u8],
        delegations: &[Vec<u8>],
    ) -> Result<Self, Refusal> {
        let added_mailbox_binding = binding_of(added_mailbox_binding, "committerDelegationError")?;
        let added_account_key = <[u8; ADDED_ACCOUNT_KEY_LEN]>::try_from(added_account_key)
            .map_err(|_| {
                length_refusal(
                    "reachError",
                    "keyLength",
                    "added account key",
                    ADDED_ACCOUNT_KEY_LEN,
                    added_account_key.len(),
                )
            })?;
        let delegations = delegations
            .iter()
            .map(|d| {
                <[u8; COMMITTER_DELEGATION_BLOB_LEN]>::try_from(d.as_slice()).map_err(|_| {
                    length_refusal(
                        "committerDelegationError",
                        "delegationMalformed",
                        "a delegation",
                        COMMITTER_DELEGATION_BLOB_LEN,
                        d.len(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            inner: RustCommitterDelegationEnvelope {
                added_account_key,
                added_mailbox_binding,
                delegations,
            },
        })
    }
}

#[wasm_bindgen]
impl CommitterDelegationEnvelope {
    /// Build an envelope from the added person's 32-byte account signature
    /// key, their 97-byte mailbox binding (ADR-0078; the `binding` of the
    /// grant the proposer holds for them) and the raw 185-byte delegation
    /// blobs (`Uint8Array[]`).
    ///
    /// # Errors
    ///
    /// Throws with `reachError` `keyLength`, or `committerDelegationError`
    /// `badBinding` / `delegationMalformed` for a blob that is not 185 bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(
        added_account_key: &[u8],
        added_mailbox_binding: &[u8],
        delegations: Vec<js_sys::Uint8Array>,
    ) -> Result<CommitterDelegationEnvelope, JsValue> {
        let owned: Vec<Vec<u8>> = delegations.into_iter().map(|a| a.to_vec()).collect();
        js(Self::new_inner(
            added_account_key,
            added_mailbox_binding,
            &owned,
        ))
    }

    /// The added person's 97-byte mailbox binding (ADR-0078), not verified
    /// here (`checkCommitterDelegations` verifies it).
    #[wasm_bindgen(getter, js_name = addedMailboxBinding)]
    #[must_use]
    pub fn added_mailbox_binding(&self) -> Vec<u8> {
        self.inner.added_mailbox_binding.encode().to_vec()
    }

    /// The added person's 32-byte account signature key.
    #[wasm_bindgen(getter, js_name = addedAccountKey)]
    #[must_use]
    pub fn added_account_key(&self) -> Vec<u8> {
        self.inner.added_account_key.to_vec()
    }

    /// How many delegations.
    #[wasm_bindgen(js_name = delegationCount)]
    #[must_use]
    pub fn delegation_count(&self) -> usize {
        self.inner.delegations.len()
    }

    /// The raw delegation blob at `index`, or `undefined` past the end.
    #[must_use]
    pub fn delegation(&self, index: usize) -> Option<Vec<u8>> {
        self.inner.delegations.get(index).map(|d| d.to_vec())
    }
}

/// One delegation, decoded (structure only; the Delivery Service checks the
/// signature and the expiry when it is used).
#[wasm_bindgen(js_name = Delegation)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmDelegation {
    inner: Delegation,
}

#[wasm_bindgen(js_class = Delegation)]
impl WasmDelegation {
    /// The 32-byte mailbox address the write may go to (ADR-0078).
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn mailbox(&self) -> Vec<u8> {
        self.inner.mailbox.to_vec()
    }

    /// The 32-byte key that signs the delegated request.
    #[wasm_bindgen(getter, js_name = delegatePk)]
    #[must_use]
    pub fn delegate_pk(&self) -> Vec<u8> {
        self.inner.delegate_pk.to_vec()
    }

    /// When the delegation stops being honoured, unix ms (a `bigint`).
    #[wasm_bindgen(getter, js_name = expiresAtMs)]
    #[must_use]
    pub fn expires_at_ms(&self) -> u64 {
        self.inner.expires_at_ms
    }

    /// The 16-byte nonce the DS consumes on use.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn nonce(&self) -> Vec<u8> {
        self.inner.nonce.to_vec()
    }

    /// The 32-byte reach public key that signed the delegation.
    #[wasm_bindgen(getter, js_name = issuerPk)]
    #[must_use]
    pub fn issuer_pk(&self) -> Vec<u8> {
        self.inner.issuer_pk.to_vec()
    }

    /// The `x-lakko-ds-delegation` header value.
    #[wasm_bindgen(js_name = headerValue)]
    #[must_use]
    pub fn header_value(&self) -> String {
        self.inner.to_header_value()
    }
}

fn compose_committer_delegation_inner(
    envelope: &CommitterDelegationEnvelope,
) -> Result<Vec<u8>, Refusal> {
    Ok(rust_compose_committer_delegation(&envelope.inner)?)
}

/// Compose a committer-delegation envelope: the plaintext of an MLS
/// application message the proposer sends in the group beside its `Add`
/// proposal (ADR-0060 §6). Structural checks only.
///
/// # Errors
///
/// Throws with `committerDelegationError` `badCount`.
#[wasm_bindgen(js_name = composeCommitterDelegationEnvelope)]
pub fn compose_committer_delegation_envelope(
    envelope: &CommitterDelegationEnvelope,
) -> Result<Vec<u8>, JsValue> {
    js(compose_committer_delegation_inner(envelope))
}

fn decode_committer_delegation_inner(
    plaintext: &[u8],
) -> Result<CommitterDelegationEnvelope, Refusal> {
    Ok(CommitterDelegationEnvelope::from_rust(
        rust_decode_committer_delegation(plaintext)?,
    ))
}

/// Strictly decode a committer-delegation envelope (structure only).
///
/// # Errors
///
/// Throws with `committerDelegationError` `wrongDomain` / `unknownVersion` /
/// `badCount` / `badLength`.
#[wasm_bindgen(js_name = decodeCommitterDelegationEnvelope)]
pub fn decode_committer_delegation_envelope(
    plaintext: &[u8],
) -> Result<CommitterDelegationEnvelope, JsValue> {
    js(decode_committer_delegation_inner(plaintext))
}

fn check_committer_delegations_inner(
    envelope: &CommitterDelegationEnvelope,
) -> Result<Vec<WasmDelegation>, Refusal> {
    Ok(rust_check_committer_delegations(&envelope.inner)?
        .into_iter()
        .map(|inner| WasmDelegation { inner })
        .collect())
}

/// Parse and check every delegation of an envelope (ADR-0060 §6): each blob
/// parses, names the added person's mailbox, and no two name the same
/// delegate. Any failure refuses the whole envelope — keep the previous one.
///
/// # Errors
///
/// Throws with `committerDelegationError` `badAddedAccountKey` /
/// `malformed` / `wrongMailbox` / `duplicateDelegate`.
#[wasm_bindgen(js_name = checkCommitterDelegations)]
pub fn check_committer_delegations(
    envelope: &CommitterDelegationEnvelope,
) -> Result<Vec<WasmDelegation>, JsValue> {
    js(check_committer_delegations_inner(envelope))
}

fn committer_delegation_for_inner(
    envelope: &CommitterDelegationEnvelope,
    own_account_key: &[u8],
) -> Result<Option<String>, Refusal> {
    let own = <[u8; ADDED_ACCOUNT_KEY_LEN]>::try_from(own_account_key).map_err(|_| {
        length_refusal(
            "reachError",
            "keyLength",
            "own account key",
            ADDED_ACCOUNT_KEY_LEN,
            own_account_key.len(),
        )
    })?;
    let parsed = rust_check_committer_delegations(&envelope.inner)?;
    Ok(rust_delegation_for(&parsed, &own).map(Delegation::to_header_value))
}

/// The committer's side of ADR-0060 §6: after its sweep added the envelope's
/// person, the delegation naming `ownAccountKey` (this device's account
/// signature key), as the `x-lakko-ds-delegation` header value to send with
/// the Welcome's mailbox write — signed with
/// `DsProofSigner.forReach(accountIdentity ed25519 seed)`, the key the
/// delegation names. `undefined` when no delegation names this account. The
/// envelope is checked first (`checkCommitterDelegations`). Use a delegation
/// only from the newest envelope kept per `(MLS sender resolved at receipt,
/// addedAccountKey)`, and only for the Welcome of a sweep that added that
/// person.
///
/// # Errors
///
/// Every error of `checkCommitterDelegations`; `reachError` `keyLength`.
#[wasm_bindgen(js_name = committerDelegationFor)]
pub fn committer_delegation_for(
    envelope: &CommitterDelegationEnvelope,
    own_account_key: &[u8],
) -> Result<Option<String>, JsValue> {
    js(committer_delegation_for_inner(envelope, own_account_key))
}

fn decode_delegation_inner(blob: &[u8]) -> Result<WasmDelegation, Refusal> {
    Delegation::decode(blob)
        .map(|inner| WasmDelegation { inner })
        .map_err(|e| delegation_refusal(&e))
}

/// Decode one raw 185-byte delegation blob (structure only).
///
/// # Errors
///
/// Throws with `committerDelegationError` `delegationMalformed` /
/// `delegationUnknownVersion`.
#[wasm_bindgen(js_name = decodeDelegation)]
pub fn decode_delegation(blob: &[u8]) -> Result<WasmDelegation, JsValue> {
    js(decode_delegation_inner(blob))
}

/// The `x-lakko-ds-delegation` header value of a raw delegation blob.
///
/// # Errors
///
/// As `decodeDelegation`.
#[wasm_bindgen(js_name = delegationHeaderValue)]
pub fn delegation_header_value(blob: &[u8]) -> Result<String, JsValue> {
    js(decode_delegation_inner(blob).map(|d| d.header_value()))
}

/// Most delegations one envelope carries: 16.
#[wasm_bindgen(js_name = maxCommitterDelegations)]
#[must_use]
pub fn max_committer_delegations() -> usize {
    MAX_COMMITTER_DELEGATIONS
}

/// Length of one delegation blob: 185.
#[wasm_bindgen(js_name = committerDelegationBlobLen)]
#[must_use]
pub fn committer_delegation_blob_len() -> usize {
    COMMITTER_DELEGATION_BLOB_LEN
}

// ============================================================================
// Username certificate — ADR-0060 §5 (verify only)
// ============================================================================

/// The authenticated claims of a verified username certificate.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsernameCertificate {
    inner: RustUsernameCertificate,
}

#[wasm_bindgen]
impl UsernameCertificate {
    /// The certified 64-byte lookup key of the username (the
    /// `discoveryLookupKey` of its OPRF output).
    #[wasm_bindgen(getter, js_name = oprfHash)]
    #[must_use]
    pub fn oprf_hash(&self) -> Vec<u8> {
        self.inner.oprf_hash().to_vec()
    }

    /// The 16-byte account the username is registered to.
    #[wasm_bindgen(getter, js_name = accountId)]
    #[must_use]
    pub fn account_id(&self) -> Vec<u8> {
        self.inner.account_id().to_vec()
    }

    /// When discovery issued it, Unix ms (a `bigint`). Informational.
    #[wasm_bindgen(getter, js_name = issuedAtUnixMillis)]
    #[must_use]
    pub fn issued_at_unix_millis(&self) -> i64 {
        self.inner.issued_at_unix_millis()
    }
}

fn verify_username_binding_inner(
    certificate: &[u8],
    verifying_key: &[u8],
    expected_oprf_hash: &[u8],
    expected_account_id: &[u8],
) -> Result<UsernameCertificate, Refusal> {
    const P: &str = "usernameCertificateError";
    let verifying = VerifyingKey::from_bytes(verifying_key).map_err(|_| {
        Refusal::new(
            P,
            "badVerifyingKey",
            "the pinned discovery key is not an Ed25519 key",
        )
    })?;
    let oprf = <[u8; OPRF_HASH_LEN]>::try_from(expected_oprf_hash).map_err(|_| {
        length_refusal(
            P,
            "oprfHashLength",
            "OPRF hash",
            OPRF_HASH_LEN,
            expected_oprf_hash.len(),
        )
    })?;
    let account = <[u8; ACCOUNT_ID_LEN]>::try_from(expected_account_id).map_err(|_| {
        length_refusal(
            P,
            "accountIdLength",
            "account id",
            ACCOUNT_ID_LEN,
            expected_account_id.len(),
        )
    })?;
    Ok(UsernameCertificate {
        inner: rust_verify_username_binding(certificate, &verifying, &oprf, &account)?,
    })
}

/// Verify a claimed username: the certificate's signature under the pinned
/// discovery key and its binding to `expectedOprfHash` (the `lookupKey` of
/// the OPRF round over the CLAIMED username, never its `output`) and
/// `expectedAccountId`. No freshness check. Show
/// the username only on success.
///
/// # Errors
///
/// Throws with `usernameCertificateError` (`badLength`, `badVersion`,
/// `badSignature`, `usernameMismatch`, `accountMismatch`,
/// `badVerifyingKey`, `oprfHashLength`, `accountIdLength`).
#[wasm_bindgen(js_name = verifyUsernameBinding)]
pub fn verify_username_binding(
    certificate: &[u8],
    verifying_key: &[u8],
    expected_oprf_hash: &[u8],
    expected_account_id: &[u8],
) -> Result<UsernameCertificate, JsValue> {
    js(verify_username_binding_inner(
        certificate,
        verifying_key,
        expected_oprf_hash,
        expected_account_id,
    ))
}

/// Wire length of a username certificate: 153.
#[wasm_bindgen(js_name = usernameCertificateLen)]
#[must_use]
pub fn username_certificate_len() -> usize {
    lakko_attest::USERNAME_CERT_LEN
}

// ============================================================================
// Tests (host; success paths through the exports, refusals through `_inner`)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    const ACCOUNT_SEED: [u8; 32] = [0x42; 32];

    /// ADR-0060 §6 through this surface (the `*_inner` halves, so refusals
    /// are checkable on the host): each candidate finds exactly its own
    /// delegation; a delegation for another mailbox refuses the envelope.
    #[test]
    fn a_committer_finds_its_own_delegation() {
        let account = |b: u8| SigningKey::from_seed(&[b; 32]).verifying_key().to_bytes();
        let added = account(9);
        let mailbox = lakko_ds_proof::MailboxKey::from_account_seed(&[9; 32]).address();
        let binding = lakko_contacts::mailbox::MailboxBinding::for_account_seed(&[9; 32])
            .encode()
            .to_vec();
        let (owner, admin) = (account(1), account(2));
        let blob = |mb: &[u8; 32], delegate: [u8; 32], n: u8| {
            lakko_ds_proof::sign_delegation(&[0x44; 32], mb, &delegate, 1_000, [n; 16])
                .encode()
                .to_vec()
        };
        let env = CommitterDelegationEnvelope::new_inner(
            &added,
            &binding,
            &[blob(&mailbox, owner, 1), blob(&mailbox, admin, 2)],
        )
        .unwrap();
        let wire = compose_committer_delegation_inner(&env).unwrap();
        let decoded = decode_committer_delegation_inner(&wire).unwrap();
        assert_eq!(decoded, env);
        assert_eq!(decoded.delegation_count(), 2);
        let dispatched = crate::contacts::dispatch_incoming_envelope_for_tests(&wire);
        assert_eq!(dispatched, Some(env.clone()));
        assert_eq!(check_committer_delegations_inner(&env).unwrap().len(), 2);
        let header = committer_delegation_for_inner(&env, &admin)
            .unwrap()
            .unwrap();
        assert_eq!(
            header,
            decode_delegation_inner(&env.delegation(1).unwrap())
                .unwrap()
                .header_value()
        );
        assert_eq!(
            committer_delegation_for_inner(&env, &account(3)).unwrap(),
            None
        );

        // A binding the added account never signed refuses the envelope.
        let foreign = lakko_contacts::mailbox::MailboxBinding::for_account_seed(&[8; 32])
            .encode()
            .to_vec();
        let forged =
            CommitterDelegationEnvelope::new_inner(&added, &foreign, &[blob(&mailbox, owner, 1)])
                .unwrap();
        assert_eq!(
            check_committer_delegations_inner(&forged).unwrap_err().code,
            "badMailboxBinding"
        );
        let bad = CommitterDelegationEnvelope::new_inner(
            &added,
            &binding,
            &[blob(&mailbox, owner, 1), blob(&[0x11; 32], admin, 2)],
        )
        .unwrap();
        let refusal = committer_delegation_for_inner(&bad, &owner).unwrap_err();
        assert_eq!(refusal.property, "committerDelegationError");
        assert_eq!(refusal.code, "wrongMailbox");
        // A person in Everyone: no delegation, the binding alone names the
        // open mailbox.
        let open = CommitterDelegationEnvelope::new_inner(&added, &binding, &[]).unwrap();
        let wire = compose_committer_delegation_inner(&open).unwrap();
        assert!(decode_committer_delegation_inner(&wire).is_ok());
        assert!(check_committer_delegations_inner(&open).unwrap().is_empty());
        let too_many: Vec<_> = (0u8..17).map(|i| blob(&mailbox, owner, i)).collect();
        let refusal = compose_committer_delegation_inner(
            &CommitterDelegationEnvelope::new_inner(&added, &binding, &too_many).unwrap(),
        )
        .unwrap_err();
        assert_eq!(refusal.code, "badCount");
        assert_eq!(max_committer_delegations(), 16);
        assert_eq!(committer_delegation_blob_len(), 185);
    }

    /// `(certificate, expected OPRF hash, expected account id, code)`.
    type Case<'a> = (&'a [u8], &'a [u8], &'a [u8], &'a str);

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
        let seed = reach_seed(&ACCOUNT_SEED, ReachClass::Link, b"link-1", 3).unwrap();
        assert_eq!(
            hex(&seed),
            "69e756d3faf95aae33944de2bdb27569fe890279b0817350e3980d23dd137cb6"
        );
        assert_eq!(
            hex(&reach_verifying_key(&seed).unwrap()),
            "f81b813554d8b791c378ffb6e836d2218f76ddf26e2e8d0be1209b0c2adac4d5"
        );
    }

    /// The core's continuity golden (ADR-0073 R.4), reproduced through this
    /// surface; the UniFFI surface pins the same literals.
    #[test]
    fn continuity_reach_seed_reproduces_the_core_golden() {
        let seed = continuity_reach_seed_inner(&[0x11; 16], 2).unwrap();
        assert_eq!(
            hex(&seed),
            "764a0db71d87bb535affdc9dc1d51df8648a26c8c670fa4708dcc6f31b5cc228"
        );
        assert_eq!(
            hex(&rust_reach_verifying_key(
                &seed32(&seed, "reach seed").unwrap()
            )),
            "c797f30bb7df3eed9ea6a65c183f7d5fc391387022a2a1150adddea3c4caf02c"
        );
        assert_ne!(seed, continuity_reach_seed_inner(&[0x11; 16], 3).unwrap());
        for bad in [&[][..], &[0x11; 15], &[0x11; 17], &[0x11; 32]] {
            let e = continuity_reach_seed_inner(bad, 2).unwrap_err();
            assert_eq!(
                (e.property, e.code.as_str()),
                ("reachError", "accountIdLength")
            );
        }
    }

    #[test]
    fn reach_seed_matches_the_core_and_grants_round_trip() {
        let seed = reach_seed(&ACCOUNT_SEED, ReachClass::Link, &[7; 8], 3).unwrap();
        let core = rust_reach_seed(&ACCOUNT_SEED, RustReachClass::Link, &[7; 8], 3).unwrap();
        assert_eq!(seed, core.as_bytes().to_vec());
        let grant = derive_reach_grant(&ACCOUNT_SEED, ReachClass::Link, &[7; 8], 3).unwrap();
        assert_eq!(grant.seed(), seed);
        assert_eq!(grant.verifying_key(), reach_verifying_key(&seed).unwrap());
        let wire = encode_reach_grant(&grant);
        assert_eq!(wire.len(), reach_grant_len());
        assert_eq!(decode_reach_grant(&wire).unwrap(), grant);
        assert_eq!(
            ReachGrant::new(ReachClass::Link, 3, &seed, &grant.binding()).unwrap(),
            grant
        );
        assert_eq!(reach_grant_len(), 134);
        // ADR-0078: the grant names the owner's mailbox, verified only under
        // the owner's account key.
        let owner = SigningKey::from_seed(&ACCOUNT_SEED)
            .verifying_key()
            .to_bytes();
        assert_eq!(
            grant.mailbox_pk_verified_inner(&owner).unwrap(),
            lakko_contacts::mailbox::MailboxKey::from_account_seed(&ACCOUNT_SEED)
                .public()
                .to_vec()
        );
        let stranger = SigningKey::from_seed(&[0x43; 32])
            .verifying_key()
            .to_bytes();
        let refusal = grant.mailbox_pk_verified_inner(&stranger).unwrap_err();
        assert_eq!(
            (refusal.property, refusal.code.as_str()),
            ("mailboxError", "badBinding")
        );
        // The numeric values are the core's wire bytes.
        for class in RustReachClass::ALL {
            assert_eq!(ReachClass::from(class) as u8, class.to_byte());
        }
        assert_eq!((reach_seed_len(), max_reach_scope_id_len()), (32, 64));
    }

    #[test]
    fn reach_refusals_carry_their_code() {
        let e = reach_seed_inner(&ACCOUNT_SEED, ReachClass::Contact, &[1], 1).unwrap_err();
        assert_eq!(
            (e.property, e.code.as_str()),
            ("reachError", "scopeIdNotEmpty")
        );
        let e = reach_seed_inner(&[0; 31], ReachClass::Fof, &[], 1).unwrap_err();
        assert_eq!(e.code, "seedLength");
        assert_eq!(
            decode_reach_grant_inner(&[0; 36]).unwrap_err().code,
            "badLength"
        );
        assert_eq!(
            ReachGrant::new_inner(ReachClass::Fof, 1, &[0; 3], &[0; 97])
                .unwrap_err()
                .code,
            "seedLength"
        );
        assert_eq!(
            ReachGrant::new_inner(ReachClass::Fof, 1, &[0; 32], &[0; 97])
                .unwrap_err()
                .code,
            "badBinding"
        );
    }

    fn payload() -> IntroductionPayload {
        let fof = derive_reach_grant(&ACCOUNT_SEED, ReachClass::Fof, &[], 1).unwrap();
        IntroductionPayload::new(
            &SigningKey::from_seed(&ACCOUNT_SEED)
                .verifying_key()
                .to_bytes(),
            &[2; 16],
            Some("Anna Weber".to_string()),
            Some("anna".to_string()),
            None,
            &fof,
        )
        .unwrap()
    }

    #[test]
    fn introduction_round_trips_through_offer_and_bundle() {
        let terms = vec![vec![5u8; 64], vec![6u8; 64]];
        let record = compose_introduction_inner(&payload(), &terms, 10).unwrap();
        let tags = introduction_tags(&record).unwrap();
        assert_eq!(tags.len(), 2 * fof_tag_len());
        assert_eq!(tags[32..].to_vec(), fof_tag(&terms[1]).unwrap());
        assert_eq!(
            open_introduction(&record, &terms[0]).unwrap(),
            Some(payload())
        );
        assert_eq!(open_introduction(&record, &[9; 64]).unwrap(), None);

        let offer = compose_introduction_offer(&record).unwrap();
        assert_eq!(decode_introduction_offer(&offer).unwrap(), record);

        let bundle = compose_introductions_bundle_inner(std::slice::from_ref(&record), 4).unwrap();
        let decoded = decode_introductions_bundle(&bundle).unwrap();
        assert_eq!(decoded.updated_at_ms(), 4);
        assert_eq!(decoded.record_count(), 1);
        assert_eq!(decoded.record(0), Some(record));
        assert_eq!(decoded.record(1), None);
        assert!(bundle_is_newer(5, 4) && !bundle_is_newer(4, 4));
        assert_eq!(bundle_size_buckets()[0], 1);
        assert_eq!((max_introduction_terms(), max_bundle_records()), (65, 1024));
        assert_eq!(introduction_oprf_output_len(), 64);
    }

    #[test]
    fn a_public_record_carries_no_grant() {
        let public = IntroductionPayload::for_public_record(
            &SigningKey::from_seed(&ACCOUNT_SEED)
                .verifying_key()
                .to_bytes(),
            &[2; 16],
            Some("Anna Weber".to_string()),
            Some("anna".to_string()),
            None,
            &lakko_contacts::mailbox::MailboxBinding::for_account_seed(&ACCOUNT_SEED).encode(),
        )
        .unwrap();
        assert_eq!(public.mailbox_binding(), payload().mailbox_binding());
        let terms = vec![vec![5u8; 64], vec![6u8; 64]];
        let record = compose_public_record_inner(&public, &terms, 3).unwrap();
        let opened = open_public_record_inner(&record, &terms[0])
            .unwrap()
            .unwrap();
        assert_eq!(opened, public);
        assert!(opened.fof().is_none());
        assert_eq!(
            open_introduction_inner(&record, &terms[0])
                .unwrap_err()
                .code,
            "grant.missing"
        );
        assert_eq!(
            compose_public_record_inner(&payload(), &terms, 1)
                .unwrap_err()
                .code,
            "grant.unexpected"
        );
        assert_eq!(
            compose_introduction_inner(&public, &terms, 1)
                .unwrap_err()
                .code,
            "grant.missing"
        );
    }

    #[test]
    fn introduction_refusals_carry_their_code() {
        let e = compose_introduction_inner(&payload(), &[], 1).unwrap_err();
        assert_eq!(
            (e.property, e.code.as_str()),
            ("introductionError", "noTerms")
        );
        let e = compose_introduction_inner(&payload(), &[vec![5; 64], vec![5; 64]], 1).unwrap_err();
        assert_eq!(e.code, "duplicateTag");
        let e = compose_introduction_inner(&payload(), &[vec![5; 63]], 1).unwrap_err();
        assert_eq!(e.code, "oprfLength");
        let fof = derive_reach_grant(&ACCOUNT_SEED, ReachClass::Fof, &[], 1).unwrap();
        let bad = IntroductionPayload::new_inner(
            &[1; 32],
            &[2; 16],
            Some("a\nb".into()),
            None,
            None,
            fof.rust().binding,
            Some(&fof),
        )
        .unwrap();
        let e = compose_introduction_inner(&bad, &[vec![5; 64]], 1).unwrap_err();
        assert_eq!(
            (e.property, e.code.as_str()),
            ("profileError", "displayName.lineBreak")
        );
        assert_eq!(
            IntroductionPayload::new_inner(
                &[1; 31],
                &[2; 16],
                None,
                None,
                None,
                fof.rust().binding,
                Some(&fof)
            )
            .unwrap_err()
            .code,
            "identityPkLength"
        );
        assert_eq!(
            decode_introduction_offer_inner(b"nope").unwrap_err().code,
            "wrongDomain"
        );
        assert_eq!(
            decode_introductions_bundle_inner(b"nope").unwrap_err().code,
            "wrongDomain"
        );
    }

    #[test]
    fn visibility_and_reach_state_round_trip() {
        let chosen = ScopeId::new(ScopeKind::Channel, vec![3; 16]);
        let setting = VisibilitySetting::new(
            VisibilityMode::Some,
            true,
            Some(GroupsForm::Chosen),
            vec![chosen.clone()],
            true,
            Some(GroupsForm::All),
            9,
        );
        let wire = compose_visibility_envelope(&setting).unwrap();
        let decoded = decode_visibility_envelope(&wire).unwrap();
        assert_eq!(decoded, setting);
        assert_eq!(decoded.chosen(), vec![chosen.clone()]);
        assert_eq!(decoded.groups(), Some(GroupsForm::Chosen));
        assert!(allows_scope(&setting, &chosen));
        assert!(!allows_scope(
            &setting,
            &ScopeId::new(ScopeKind::Group, vec![3; 16])
        ));
        assert!(allows_contacts_of_contacts(&setting));
        assert!(visibility_is_newer(10, 9) && !visibility_is_newer(9, 9));
        assert_eq!(setting.with_updated_at_ms(12).updated_at_ms(), 12);
        assert_eq!(default_visibility_setting().mode(), VisibilityMode::Some);

        let state = ReachState::new(
            vec![
                ReachStateEntry::new(ReachClass::Contact, vec![], 2),
                ReachStateEntry::new(ReachClass::Link, vec![1; 8], 5),
            ],
            11,
        );
        let wire = compose_reach_state_envelope(&state).unwrap();
        let decoded = decode_reach_state_envelope(&wire).unwrap();
        assert_eq!(decoded, state);
        assert_eq!(decoded.entries().len(), 2);
        assert_eq!(
            reach_state_version_of(&state, ReachClass::Link, &[1; 8]),
            Some(5)
        );
        assert_eq!(reach_state_version_of(&state, ReachClass::Fof, &[]), None);
        assert!(reach_state_is_newer(12, 11) && !reach_state_is_newer(11, 11));
        assert_eq!(
            (
                max_chosen_scopes(),
                max_visibility_scope_id_len(),
                max_reach_state_entries()
            ),
            (1024, 64, 4096)
        );
    }

    #[test]
    fn visibility_refusals_carry_their_code() {
        let bad = VisibilitySetting::new(VisibilityMode::Some, false, None, vec![], false, None, 1);
        let e = Refusal::from(rust_compose_visibility(bad.rust()).unwrap_err());
        assert_eq!(
            (e.property, e.code.as_str()),
            ("visibilityError", "someWithNoPart")
        );
        assert_eq!(
            decode_visibility_inner(b"x").unwrap_err().code,
            "wrongDomain"
        );
        let state = ReachState::new(vec![ReachStateEntry::new(ReachClass::Link, vec![], 1)], 1);
        let e = Refusal::from(rust_compose_reach_state(&state.inner).unwrap_err());
        assert_eq!(e.code, "reach.scopeIdEmpty");
        assert_eq!(
            decode_reach_state_inner(b"x").unwrap_err().code,
            "wrongDomain"
        );
    }

    #[test]
    fn username_binding_verifies_and_refuses_with_codes() {
        let key = SigningKey::from_seed(&[0x11; 32]);
        let cert =
            RustUsernameCertificate::new([0xAB; 64], [0xCD; 16], 1_700_000_000_000).sign(&key);
        let vk = key.verifying_key().to_bytes();
        let ok = verify_username_binding(&cert, &vk, &[0xAB; 64], &[0xCD; 16]).unwrap();
        assert_eq!(ok.issued_at_unix_millis(), 1_700_000_000_000);
        assert_eq!(ok.account_id(), vec![0xCD; 16]);
        assert_eq!(ok.oprf_hash(), vec![0xAB; 64]);

        let cases: [Case<'_>; 5] = [
            (&cert, &[0xAC; 64], &[0xCD; 16], "usernameMismatch"),
            (&cert, &[0xAB; 64], &[0xCE; 16], "accountMismatch"),
            (&cert[..152], &[0xAB; 64], &[0xCD; 16], "badLength"),
            (&cert, &[0xAB; 63], &[0xCD; 16], "oprfHashLength"),
            (&cert, &[0xAB; 64], &[0xCD; 15], "accountIdLength"),
        ];
        for (c, h, a, code) in cases {
            let e = verify_username_binding_inner(c, &vk, h, a).unwrap_err();
            assert_eq!(
                (e.property, e.code.as_str()),
                ("usernameCertificateError", code)
            );
        }
        let mut tampered = cert.to_vec();
        tampered[20] ^= 1;
        assert_eq!(
            verify_username_binding_inner(&tampered, &vk, &[0xAB; 64], &[0xCD; 16])
                .unwrap_err()
                .code,
            "badSignature"
        );
        assert_eq!(username_certificate_len(), 153);
    }

    /// Same literals as `lakko-ffi-uniffi`'s table.
    #[test]
    fn username_cert_codes_are_frozen() {
        let table = [
            (UsernameCertError::BadLength { got: 1 }, "badLength"),
            (UsernameCertError::BadVersion(2), "badVersion"),
            (UsernameCertError::BadSignature, "badSignature"),
            (UsernameCertError::UsernameMismatch, "usernameMismatch"),
            (UsernameCertError::AccountMismatch, "accountMismatch"),
        ];
        for (e, code) in table {
            assert_eq!(username_cert_code(&e), code);
        }
    }
}
