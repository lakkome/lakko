// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS-layer wasm-bindgen mirror — `Identity`, `Group`, `AddMember`,
//! `ProcessedMessage`. Phase 1.13a content; the rest of the FFI
//! surface (sync / push / presence / contacts / storage) lives in
//! sibling modules and is wired through `lib.rs`.

use std::sync::Mutex;

use lakko_mls::group::MemberAddition;
use lakko_mls::{
    group, key_package, ChannelId as MlsChannelId, DeviceGroup as MlsDeviceGroup,
    Group as MlsGroup, Identity as MlsIdentity, Incoming as MlsIncoming,
    PublisherGroup as MlsPublisherGroup, RefusalReason as MlsRefusalReason, Role as MlsRole,
    RoleTable as MlsRoleTable,
};
use wasm_bindgen::prelude::*;

/// A failure on its way out to JS: the sentence, plus the machine-readable
/// membership-refusal reason when there is one.
///
/// **Why this exists rather than a bare `String`.** Everything in this file
/// used to flatten a `lakko_mls::Error` to its `Display` text, so a web client
/// wanting one of the fixed refusal strings the UX spec specifies had exactly
/// one way to tell them apart: substring-match the sentence the core happened
/// to write. That is a drift class, not a mechanism. The reason now rides
/// beside the message all the way to the thrown object.
///
/// It is a plain Rust value rather than a `JsValue` because `js_sys` cannot run
/// off `wasm32`, and the host suite is where a mapping mistake would actually
/// be caught. [`Self::into_js`] is the last hop and the only wasm-only part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MlsFailure {
    /// The human-readable sentence — becomes `err.message`.
    pub(crate) message: String,
    /// Why a membership operation was refused, or `None` when this failure is
    /// not a refusal at all (a codec failure, an OpenMLS validation failure, a
    /// poisoned lock). `None` is meaningful: it tells a client "this is not a
    /// permission answer", which it renders with its fail-closed fallback.
    pub(crate) reason: Option<MlsRefusalReason>,
}

impl From<lakko_mls::Error> for MlsFailure {
    fn from(e: lakko_mls::Error) -> Self {
        Self {
            reason: e.refusal_reason(),
            message: e.to_string(),
        }
    }
}

impl From<String> for MlsFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            reason: None,
        }
    }
}

impl From<&str> for MlsFailure {
    fn from(message: &str) -> Self {
        Self {
            message: message.to_string(),
            reason: None,
        }
    }
}

impl MlsFailure {
    /// Build the JS value that is actually thrown: a real `Error` (so
    /// `instanceof Error`, `.message` and a stack all work as before) carrying
    /// two extra own properties when this is a membership refusal —
    /// `refusalReason` (the stable code) and `refusalDisposition`
    /// (`"retryLater"` / `"doNotRetry"` / `"notPermitted"`).
    ///
    /// Properties rather than a message prefix: a caller reads
    /// `err.refusalReason`, it parses nothing, and adding the fields breaks no
    /// existing handler that only looks at `message`.
    fn into_js(self) -> JsValue {
        let err = js_sys::Error::new(&self.message);
        if let Some(reason) = self.reason {
            let target: &JsValue = err.as_ref();
            // Best-effort: a failed property write must not replace the error
            // being reported with a different one. The caller still gets the
            // sentence, and falls back to its fail-closed copy.
            let _ = js_sys::Reflect::set(
                target,
                &JsValue::from_str("refusalReason"),
                &JsValue::from_str(reason.code()),
            );
            let _ = js_sys::Reflect::set(
                target,
                &JsValue::from_str("refusalDisposition"),
                &JsValue::from_str(reason.disposition().code()),
            );
        }
        err.into()
    }
}

/// A plain thrown `Error` with `message`, for failures that are not
/// `lakko_mls::Error`s.
fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

/// Map a `lakko_mls::Error` to the value thrown in JS: an `Error` with the
/// message preserved, plus `refusalReason` / `refusalClass` when it is a
/// membership refusal (ADR-0045 §9.1 item 1).
fn mls_err(e: lakko_mls::Error) -> JsValue {
    MlsFailure::from(e).into_js()
}

/// What the caller should do about a membership-refusal code —
/// `"retryLater"`, `"doNotRetry"` or `"notPermitted"`, and `null` for an
/// unknown code.
///
/// **Three-valued, and the middle value is the point.** "Not permitted" and
/// "retry later" are the obvious two, and they leave out the commonest case: a
/// request that is allowed but cannot be accepted **as formed**, ever. Retrying
/// that loops forever. The rule is: retryable exactly when a bounded queue was
/// full.
///
/// The thrown error already carries `refusalDisposition`, so this is for a
/// caller that kept a code and wants to re-ask later. Ask it rather than
/// deciding from the code's spelling: the rule lives in the core so it cannot
/// drift between web, iOS and Android.
#[wasm_bindgen(js_name = membershipRefusalDisposition)]
#[must_use]
pub fn membership_refusal_disposition(reason: &str) -> Option<String> {
    refusal_reason_from_code(reason).map(|r| r.disposition().code().to_string())
}

/// `true` when the **identical** call will succeed once a committer has swept,
/// with the caller doing nothing else — i.e. when the UI may offer a plain
/// retry. An unknown code is `false`, which is the fail-closed direction: no
/// retry is offered for a refusal this build does not understand.
#[wasm_bindgen(js_name = membershipRefusalRetryMaySucceed)]
#[must_use]
pub fn membership_refusal_retry_may_succeed(reason: &str) -> bool {
    refusal_reason_from_code(reason).is_some_and(MlsRefusalReason::retry_may_succeed)
}

/// Look a reason up by its stable code. Not a parser of prose — the code is
/// the value the thrown error handed the caller.
fn refusal_reason_from_code(code: &str) -> Option<MlsRefusalReason> {
    MlsRefusalReason::ALL.into_iter().find(|r| r.code() == code)
}

/// Build an [`MlsRoleTable`] from the flat key material the wasm surface
/// carries, refusing anything that is not a whole number of 32-byte keys.
///
/// Shared by `Group.setGroupRoles` and `Group.proposeGroupRoles` so the two
/// paths cannot drift on what they accept. Returns a plain message rather than
/// a `JsError` because `JsError` can only be constructed on the wasm target —
/// see the ADR-0045 core impl block on [`Group`].
fn role_table_from_flat(owner: &[u8], admins_flat: &[u8]) -> Result<MlsRoleTable, &'static str> {
    let owner: [u8; 32] = owner.try_into().map_err(|_| "owner key must be 32 bytes")?;
    if admins_flat.len() % 32 != 0 {
        return Err("admins must be a multiple of 32 bytes");
    }
    let mut admins = Vec::with_capacity(admins_flat.len() / 32);
    for chunk in admins_flat.chunks_exact(32) {
        let mut k = [0u8; 32];
        k.copy_from_slice(chunk);
        admins.push(k);
    }
    Ok(MlsRoleTable::new(owner, admins))
}

// ============================================================================
// Identity
// ============================================================================

/// Lakko identity — owns the device's signing keypair, MLS credential
/// and per-device storage provider. Mirror of UniFFI's `Identity`.
///
/// JS usage:
///
/// ```js
/// import { Identity } from "@lakko/ffi-wasm";
/// const alice = new Identity("alice");
/// const pk = alice.signaturePublicKey;        // Uint8Array
/// const kp = alice.createKeyPackage();        // Uint8Array
/// ```
#[wasm_bindgen]
pub struct Identity {
    inner: MlsIdentity,
}

impl Identity {
    /// The core identity, for sibling modules that need the seed material
    /// (ADR-0053 mailbox signer). Crate-private on purpose.
    pub(crate) const fn mls_identity(&self) -> &MlsIdentity {
        &self.inner
    }
}

#[wasm_bindgen]
impl Identity {
    /// Generate a fresh identity with the given user-display label.
    #[wasm_bindgen(constructor)]
    pub fn new(name: &str) -> Result<Identity, JsValue> {
        let inner = MlsIdentity::generate(name).map_err(mls_err)?;
        Ok(Self { inner })
    }

    /// Derive an identity **deterministically** from a seed.
    ///
    /// Same `seed` ⇒ byte-identical Ed25519 + X25519 keys. The
    /// expected caller is the `apps/web` auth layer, which
    /// seeds this with the OPAQUE password export key so a
    /// fresh login on any device reproduces the user's
    /// long-lived public keys without persisting Identity bytes
    /// anywhere.
    #[wasm_bindgen(js_name = fromSeed)]
    pub fn from_seed(seed: &[u8]) -> Result<Identity, JsValue> {
        let inner = MlsIdentity::from_seed(seed).map_err(mls_err)?;
        Ok(Self { inner })
    }

    /// A **device** identity bound to an account (ADR-0050): derive it from
    /// `seed` and mint its device binding under `accountSeed` (the account
    /// identity's `ed25519Seed`). Every KeyPackage and every group this
    /// identity creates carries the binding in its leaf credential.
    #[wasm_bindgen(js_name = fromSeedBound)]
    pub fn from_seed_bound(
        seed: &[u8],
        account_seed: &[u8],
        issued_at_unix_secs: u64,
    ) -> Result<Identity, JsValue> {
        let account: [u8; 32] = account_seed
            .try_into()
            .map_err(|_| js_error("account seed must be 32 bytes"))?;
        let inner =
            MlsIdentity::from_seed_bound(seed, &account, issued_at_unix_secs).map_err(mls_err)?;
        Ok(Self { inner })
    }

    /// Rebuild a bound device identity from its persisted seed and its
    /// persisted binding bytes (ADR-0050), so `issuedAt` stays what it was.
    #[wasm_bindgen(js_name = fromSeedWithBinding)]
    pub fn from_seed_with_binding(seed: &[u8], binding: &[u8]) -> Result<Identity, JsValue> {
        let inner = MlsIdentity::from_seed_with_binding(seed, binding).map_err(mls_err)?;
        Ok(Self { inner })
    }

    /// Bind this identity to an account in place (ADR-0050); see
    /// `fromSeedBound`.
    #[wasm_bindgen(js_name = bindToAccount)]
    pub fn bind_to_account(
        &mut self,
        account_seed: &[u8],
        issued_at_unix_secs: u64,
    ) -> Result<(), JsValue> {
        let account: [u8; 32] = account_seed
            .try_into()
            .map_err(|_| js_error("account seed must be 32 bytes"))?;
        self.inner
            .bind_to_account(&account, issued_at_unix_secs)
            .map_err(mls_err)
    }

    /// The wire bytes of this identity's ADR-0050 device binding, or
    /// `undefined` for an unbound identity. Persist them beside the device
    /// seed.
    #[wasm_bindgen(getter, js_name = deviceBinding)]
    pub fn device_binding(&self) -> Option<Vec<u8>> {
        self.inner.device_binding().map(<[u8]>::to_vec)
    }

    /// The account key this identity acts for (ADR-0050): the binding's
    /// account key when bound, else its own signature key.
    #[wasm_bindgen(getter, js_name = accountPublicKey)]
    pub fn account_public_key(&self) -> Option<Vec<u8>> {
        self.inner.account_public_key().map(|k| k.to_vec())
    }

    /// Public signature key bytes.
    #[wasm_bindgen(getter, js_name = signaturePublicKey)]
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.inner.signature_public_key().to_vec()
    }

    /// Long-lived X25519 public key (32 bytes) — the HPKE
    /// init_key callers of `lakko-contacts::compose` target when
    /// sealing an envelope to this identity. Pair with the bundle
    /// uploaded to the discovery service via
    /// `POST /discovery/identity` (Phase 1.16d3a).
    #[wasm_bindgen(getter, js_name = x25519PublicKey)]
    pub fn x25519_public_key(&self) -> Vec<u8> {
        self.inner.x25519_public_key().to_vec()
    }

    /// Raw X25519 secret bytes (32 bytes). Used as input to
    /// `ContactRequest.openEnvelope(...)` when decrypting an
    /// incoming envelope.
    ///
    /// **Sensitive.** The caller MUST persist these bytes only
    /// after wrapping them with the device root key (1.16c3c2's
    /// `deriveDeviceRootKey` output). They MUST NOT touch
    /// `localStorage` or any other unencrypted store.
    #[wasm_bindgen(getter, js_name = x25519SecretKey)]
    pub fn x25519_secret_key(&self) -> Vec<u8> {
        self.inner.x25519_secret_bytes().to_vec()
    }

    /// Raw Ed25519 signing-seed bytes (32 bytes). Hand this to
    /// `ContactRequest.compose(...)` as the
    /// `senderSigningSeed` argument when sending a contact
    /// request. Same secret-handling rules as
    /// [`x25519SecretKey`](Self::x25519_secret_key).
    #[wasm_bindgen(getter, js_name = ed25519Seed)]
    pub fn ed25519_seed(&self) -> Vec<u8> {
        self.inner.ed25519_seed_bytes().to_vec()
    }

    /// Serialise the OpenMLS provider's storage to opaque
    /// bytes (Phase 1.16e6b). Pair with
    /// [`Identity::restoreStorage`](Self::restore_storage)
    /// to round-trip MLS state across a page reload.
    ///
    /// The bytes are opaque to the JS caller — the only
    /// supported operation is feeding them back through
    /// `restoreStorage` on a fresh `Identity` derived from the
    /// same seed. apps/web wraps the snapshot under the device
    /// root key (AES-256-GCM, 1.16c3c2) before stashing in
    /// IndexedDB so the on-disk bytes can't be lifted by
    /// page-level JavaScript.
    #[wasm_bindgen(js_name = snapshotStorage)]
    pub fn snapshot_storage(&self) -> Result<Vec<u8>, JsValue> {
        self.inner.snapshot_storage().map_err(mls_err)
    }

    /// Replace this identity's MLS provider storage with the
    /// contents of a snapshot produced by `snapshotStorage`
    /// on a previous run. **Replaces** rather than merges —
    /// see the Rust-side docstring for the seed-pairing
    /// requirement.
    #[wasm_bindgen(js_name = restoreStorage)]
    pub fn restore_storage(&self, snapshot: &[u8]) -> Result<(), JsValue> {
        self.inner.restore_storage(snapshot).map_err(mls_err)
    }

    /// Create a fresh KeyPackage in TLS wire form.
    #[wasm_bindgen(js_name = createKeyPackage)]
    pub fn create_key_package(&self) -> Result<Vec<u8>, JsValue> {
        let kp = self.inner.create_key_package().map_err(mls_err)?;
        key_package::serialize(&kp).map_err(mls_err)
    }

    /// Create an external-join proposal — the joiner side of ADR-0027 (G-3).
    ///
    /// A non-member crafts an external Add proposal for a group's current
    /// `group_id` + `epoch` (learned out-of-band via a join ticket). The
    /// returned wire bytes are posted to the group journal; an owner/admin
    /// approves via `Group.approveExternalJoin`, and whichever device then
    /// sweeps produces the Welcome the joiner opens via
    /// `Group.joinFromWelcome`. The proposal embeds a fresh KeyPackage
    /// whose private keys persist in this identity's provider, so the SAME
    /// identity must later open the Welcome.
    #[wasm_bindgen(js_name = createExternalJoinProposal)]
    pub fn create_external_join_proposal(
        &self,
        group_id: &[u8],
        epoch: u64,
    ) -> Result<Vec<u8>, JsValue> {
        self.inner
            .create_external_join_proposal(group_id, epoch)
            .map_err(mls_err)
    }
}

/// Extract the 32-byte leaf Ed25519 signature key from a wire-form KeyPackage —
/// the stable per-identity key distinct KeyPackages minted by the same identity
/// share (only the HPKE init key is fresh per KP). Used by the call-admission
/// dedup (ADR-0032 / G-22) so a peer re-minting fresh KeyPackages under one
/// identity is admitted at most once.
///
/// # Errors
///
/// `JsError` on a KeyPackage parse failure.
#[wasm_bindgen(js_name = keyPackageSignatureKey)]
pub fn key_package_signature_key(key_package: &[u8]) -> Result<Vec<u8>, JsValue> {
    key_package::signature_key(key_package).map_err(mls_err)
}

/// Resolve a wire-form KeyPackage's leaf to the **account** it belongs to
/// (ADR-0050): the device binding's account key, the leaf key itself for an
/// unbound Phase 1 leaf, or `undefined` for a leaf whose binding does not
/// verify. The call-admission dedup and a contact's device list key on this.
#[wasm_bindgen(js_name = keyPackageAccountKey)]
pub fn key_package_account_key(key_package: &[u8]) -> Result<Option<Vec<u8>>, JsValue> {
    key_package::account_key(key_package)
        .map(|k| k.map(|k| k.to_vec()))
        .map_err(mls_err)
}

// ============================================================================
// AddMember (record)
// ============================================================================

/// Result of [`Group::add_member`].
///
/// Distribute `commit` to all existing members for processing; deliver
/// `welcome` to the newly-added member.
#[wasm_bindgen]
pub struct AddMember {
    commit_bytes: Vec<u8>,
    welcome_bytes: Vec<u8>,
}

#[wasm_bindgen]
impl AddMember {
    /// Encoded MLS commit message — broadcast to existing members.
    #[wasm_bindgen(getter)]
    pub fn commit(&self) -> Vec<u8> {
        self.commit_bytes.clone()
    }

    /// Encoded MLS welcome message — sent to the joining member.
    #[wasm_bindgen(getter)]
    pub fn welcome(&self) -> Vec<u8> {
        self.welcome_bytes.clone()
    }
}

// ============================================================================
// GroupMember (record) — Phase 1.18h
// ============================================================================

/// One member's identity snapshot at the current epoch. Mirror
/// of [`lakko_mls::MemberRecord`].
///
/// The JS layer reads `leafIndex` to feed into
/// `Group.removeMember(...)` for kick UX and
/// `signaturePublicKey` to look the member up in the contacts
/// cache / render a fingerprint. `leafIndex` is stable inside a
/// single epoch but can renumber across an add/remove cycle —
/// callers MUST NOT cache it past a `processIncoming` commit.
#[wasm_bindgen]
pub struct GroupMember {
    leaf_index_val: u32,
    signature_public_key_val: Vec<u8>,
    account: Vec<u8>,
}

#[wasm_bindgen]
impl GroupMember {
    /// OpenMLS leaf position at the current epoch.
    #[wasm_bindgen(getter, js_name = leafIndex)]
    pub fn leaf_index(&self) -> u32 {
        self.leaf_index_val
    }

    /// 32-byte Ed25519 verifying key of the member's **leaf** (its device).
    #[wasm_bindgen(getter, js_name = signaturePublicKey)]
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.signature_public_key_val.clone()
    }

    /// The **account** key the leaf belongs to (ADR-0050) — what the UI
    /// matches against the contacts list to render a name, so one person with
    /// three devices is one person. Empty when the leaf's credential did not
    /// resolve (never admitted by a Lakko gate).
    #[wasm_bindgen(getter, js_name = accountKey)]
    pub fn account_key(&self) -> Vec<u8> {
        self.account.clone()
    }
}

// ============================================================================
// ProcessedMessage (tagged result of processIncoming)
// ============================================================================

/// Discriminator for [`ProcessedMessage`]. wasm-bindgen renders this
/// as a JS number-enum; we expose a string discriminator via
/// [`ProcessedMessage::kind`] for ergonomic `switch` / `case`.
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessedMessageKind {
    /// Application payload (decrypted plaintext available via
    /// `applicationData`).
    Application,
    /// A commit was processed and merged. Group state advanced.
    Commit,
    /// A standalone proposal was processed (no state advance).
    Proposal,
    /// External-join proposal was processed.
    ExternalJoinProposal,
}

/// Tagged result of [`Group::process_incoming`].
///
/// JS code reads `kind` (a string) to discriminate, then
/// `applicationData` for [`ProcessedMessageKind::Application`].
/// We deliberately don't expose a sum-type on the JS side because
/// wasm-bindgen has no idiomatic story for Rust enums with
/// associated data; a tagged class is what TypeScript would idiom
/// to anyway.
#[wasm_bindgen]
pub struct ProcessedMessage {
    kind: ProcessedMessageKind,
    application_data: Option<Vec<u8>>,
    sender_leaf: Option<u32>,
    sender_account: Option<Vec<u8>>,
    external_join_signature_key: Option<Vec<u8>>,
    external_join_retained: bool,
    proposal: Option<ProposalRecord>,
}

impl ProcessedMessage {
    /// Construct an Application result carrying the sender's leaf
    /// (ADR-0023).
    fn application(
        data: Vec<u8>,
        sender_leaf: Option<u32>,
        sender_account: Option<Vec<u8>>,
    ) -> Self {
        Self {
            kind: ProcessedMessageKind::Application,
            application_data: Some(data),
            sender_leaf,
            sender_account,
            external_join_signature_key: None,
            external_join_retained: false,
            proposal: None,
        }
    }

    /// Construct a control-message result (commit / proposal) — no
    /// payload, no sender leaf.
    const fn control(kind: ProcessedMessageKind) -> Self {
        Self {
            kind,
            application_data: None,
            sender_leaf: None,
            sender_account: None,
            external_join_signature_key: None,
            external_join_retained: false,
            proposal: None,
        }
    }

    /// Construct an ADR-0045 proposal result carrying the full record — what
    /// was proposed, by which leaf, and whether the receipt gate stored it.
    fn from_proposal(record: lakko_mls::ProposalRecord) -> Self {
        Self {
            kind: ProcessedMessageKind::Proposal,
            application_data: None,
            sender_leaf: record.sender_leaf,
            sender_account: None,
            external_join_signature_key: None,
            external_join_retained: false,
            proposal: Some(record.into()),
        }
    }

    /// Construct an external-join-proposal result carrying the joiner's
    /// signature key and whether the stranger quarantine kept it (ADR-0027,
    /// G-3).
    fn external_join(joiner_signature_key: Vec<u8>, retained: bool) -> Self {
        Self {
            kind: ProcessedMessageKind::ExternalJoinProposal,
            application_data: None,
            sender_leaf: None,
            sender_account: None,
            external_join_signature_key: Some(joiner_signature_key),
            external_join_retained: retained,
            proposal: None,
        }
    }
}

#[wasm_bindgen]
impl ProcessedMessage {
    /// Discriminator. `"Application" | "Commit" | "Proposal" |
    /// "ExternalJoinProposal"`. Useful for `switch` / `case` in
    /// TypeScript.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        match self.kind {
            ProcessedMessageKind::Application => "Application".into(),
            ProcessedMessageKind::Commit => "Commit".into(),
            ProcessedMessageKind::Proposal => "Proposal".into(),
            ProcessedMessageKind::ExternalJoinProposal => "ExternalJoinProposal".into(),
        }
    }

    /// Application plaintext, present iff `kind == "Application"`.
    #[wasm_bindgen(getter, js_name = applicationData)]
    pub fn application_data(&self) -> Option<Vec<u8>> {
        self.application_data.clone()
    }

    /// The sender's leaf index for an Application message (ADR-0023),
    /// or `undefined` for a control message / external sender. The
    /// chat receive path uses it for the author-only edit/delete gate
    /// + reaction attribution.
    #[wasm_bindgen(getter, js_name = senderLeaf)]
    pub fn sender_leaf(&self) -> Option<u32> {
        self.sender_leaf
    }

    /// The **account** key the sender leaf resolves to (ADR-0050), or
    /// `undefined` for a control message, an external sender or an
    /// unresolvable leaf. This — never the leaf — is what authorship,
    /// reactions and read state key on, so one person is one author across
    /// all their devices.
    #[wasm_bindgen(getter, js_name = senderAccount)]
    pub fn sender_account(&self) -> Option<Vec<u8>> {
        self.sender_account.clone()
    }

    /// The prospective joiner's signature key, present iff
    /// `kind == "ExternalJoinProposal"` (ADR-0027, G-3). The handle the caller
    /// passes to `approveExternalJoin` / `rejectExternalJoin`.
    #[wasm_bindgen(getter, js_name = externalJoinSignatureKey)]
    pub fn external_join_signature_key(&self) -> Option<Vec<u8>> {
        self.external_join_signature_key.clone()
    }

    /// Whether an external-join request was **retained**, so an approve can act
    /// on it. Meaningful only when `kind === "ExternalJoinProposal"`; `false`
    /// for every other kind.
    ///
    /// **`false` means an approve would fail, so do not offer one.** The event
    /// fires either way — a request arriving while the stranger quarantine is
    /// full is surfaced but not kept, because a stranger must not be able to
    /// make a member's receive loop error. Rendering an unretained request as
    /// actionable is exactly the shape that offers a user an action that cannot
    /// work. The remedy belongs to the joiner (ask again later), not the admin.
    #[wasm_bindgen(getter, js_name = externalJoinRetained)]
    #[must_use]
    pub fn external_join_retained(&self) -> bool {
        self.external_join_retained
    }

    /// The ADR-0045 proposal record, present **iff** `kind === "Proposal"` and
    /// `undefined` otherwise.
    ///
    /// Read `proposal.stored` before treating it as a pending request:
    /// `false` means the receipt gate **refused** it and no sweep will ever
    /// commit it. `senderLeaf` is also mirrored onto this `ProcessedMessage`
    /// for a uniform "who sent this" read across kinds.
    #[wasm_bindgen(getter)]
    pub fn proposal(&self) -> Option<ProposalRecord> {
        self.proposal.clone()
    }
}

// ============================================================================
// ADR-0045 proposal surface
// ============================================================================

/// One ADR-0045 proposal as the receive path saw it. wasm-bindgen mirror of
/// UniFFI's `ProposalRecord` / [`lakko_mls::ProposalRecord`].
///
/// # Why a flat tagged class rather than a sum type
///
/// wasm-bindgen has no idiomatic story for Rust enums carrying data, so this
/// follows the two shapes the repo already uses for exactly this problem:
/// [`ProcessedMessage`] (a string `kind` discriminator plus per-variant
/// optional getters) and `ParsedBody` in `attachments.rs`. The alternative
/// precedent — `SfuServerFrame`, where the web reads fields off a TS type over
/// a native `JSON.parse` — does not apply: those frames arrive as JSON on the
/// wire already, whereas this record is produced by the core, so routing it
/// through a JSON round-trip would invent a serialisation that nothing else
/// needs.
///
/// # The payload fields are per-variant, never shared
///
/// `ParsedBody` reuses one `target` slot across `edit` / `delete` / `reaction`
/// because every one of those means the same thing (a message id). The two
/// payloads here do **not**: an `Add` carries a 32-byte signature key and a
/// `Remove` carries a leaf index. They therefore get **separate, differently
/// named** getters — [`Self::add_signature_key`] and
/// [`Self::remove_target_leaf`] — each `undefined` outside its own variant, so
/// a caller cannot read a `Remove`'s target as if it were an `Add`'s key.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct ProposalRecord {
    kind: &'static str,
    sender_leaf: Option<u32>,
    stored: bool,
    add_signature_key: Option<Vec<u8>>,
    remove_target_leaf: Option<u32>,
}

impl From<lakko_mls::ProposalRecord> for ProposalRecord {
    fn from(r: lakko_mls::ProposalRecord) -> Self {
        let (kind, add_signature_key, remove_target_leaf) = match r.kind {
            lakko_mls::ProposalKind::Add { signature_key, .. } => {
                ("add", Some(signature_key), None)
            },
            lakko_mls::ProposalKind::Remove { target_leaf } => ("remove", None, Some(target_leaf)),
            lakko_mls::ProposalKind::Leave => ("leave", None, None),
            lakko_mls::ProposalKind::Update => ("update", None, None),
            lakko_mls::ProposalKind::RoleChange => ("role-change", None, None),
            lakko_mls::ProposalKind::Unsupported => ("unsupported", None, None),
        };
        Self {
            kind,
            sender_leaf: r.sender_leaf,
            stored: r.stored,
            add_signature_key,
            remove_target_leaf,
        }
    }
}

#[wasm_bindgen]
impl ProposalRecord {
    /// Discriminator: `"add" | "remove" | "leave" | "update" | "role-change" |
    /// "unsupported"`. Useful for `switch` / `case` in TypeScript.
    ///
    /// Two kinds always arrive with `stored === false`, because the core's
    /// authorisation rule refuses them for every member (fail-closed, invariant
    /// #4). `"unsupported"` is a proposal type Lakko never emits. `"update"` is
    /// an RFC 9420 leaf rotation, which openmls 0.8.1 does not bind to the
    /// sender's existing signature key — so it can rotate the very key the
    /// ADR-0022 role table is keyed on, and it is the one kind whose proposal
    /// reference is fresh on every emission, which made it floodable past the
    /// pending-set cap. A Lakko member rotates its leaf with `selfUpdate`, which
    /// composes no proposal at all.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind.to_string()
    }

    /// The proposing member's leaf index, or `undefined` for a sender that is
    /// not a resolvable group member (always unauthorised — fail-closed).
    #[wasm_bindgen(getter, js_name = senderLeaf)]
    pub fn sender_leaf(&self) -> Option<u32> {
        self.sender_leaf
    }

    /// `true` when the proposal was authorised and retained in the pending set
    /// so a later sweep can commit it; `false` when it was **dropped**.
    ///
    /// A dropped proposal is a **refusal**, not a failure to parse: a proposal
    /// does not advance the epoch, so dropping it unstored *is* the refusal, and
    /// the core reports it as data rather than throwing. Surface or log it
    /// distinctly from a stored one — a stored proposal is a pending request
    /// awaiting a sweep, a dropped one will never be committed.
    #[wasm_bindgen(getter)]
    pub fn stored(&self) -> bool {
        self.stored
    }

    /// The prospective member's 32-byte Ed25519 signature public key, present
    /// **iff** `kind === "add"` and `undefined` for every other kind.
    #[wasm_bindgen(getter, js_name = addSignatureKey)]
    pub fn add_signature_key(&self) -> Option<Vec<u8>> {
        self.add_signature_key.clone()
    }

    /// The leaf index the proposal removes, present **iff** `kind ===
    /// "remove"` and `undefined` for every other kind.
    #[wasm_bindgen(getter, js_name = removeTargetLeaf)]
    pub fn remove_target_leaf(&self) -> Option<u32> {
        self.remove_target_leaf
    }
}

/// Result of [`Group::commit_pending_proposals`] — the ADR-0045 sweep.
/// wasm-bindgen mirror of UniFFI's `ProposalCommit`.
///
/// Mirrors [`AddMember`]'s shape so ADR-0040's durable pending-commit outbox
/// wraps it unchanged, with two differences the proposal path forces: the
/// Welcome is **optional** (a sweep that carried no `add` produces none), and
/// `swept` names what the commit actually carried so the caller can mark those
/// requests applied.
#[wasm_bindgen]
pub struct ProposalCommit {
    commit_bytes: Vec<u8>,
    welcome_bytes: Option<Vec<u8>>,
    swept_records: Vec<ProposalRecord>,
}

#[wasm_bindgen]
impl ProposalCommit {
    /// Encoded MLS commit message — broadcast to all existing members.
    #[wasm_bindgen(getter)]
    pub fn commit(&self) -> Vec<u8> {
        self.commit_bytes.clone()
    }

    /// Encoded MLS welcome message — delivered to the newly added members.
    /// `undefined` unless the sweep carried at least one `add`.
    #[wasm_bindgen(getter)]
    pub fn welcome(&self) -> Option<Vec<u8>> {
        self.welcome_bytes.clone()
    }

    /// The proposals this commit carried, in sweep order, as a flat array of
    /// [`ProposalRecord`] instances (same convention as `Group.members()` —
    /// wasm-bindgen cannot return a `Vec` of a custom struct through a getter,
    /// so this is a method, not a property).
    ///
    /// Read back off the staged commit by the core, so it reports what the
    /// commit really carries rather than what the caller predicted it would.
    #[wasm_bindgen]
    pub fn swept(&self) -> Vec<JsValue> {
        self.swept_records
            .iter()
            .map(|r| JsValue::from(r.clone()))
            .collect()
    }
}

/// Result of [`Group::stage_pending_proposals`] — an ADR-0045 sweep that has
/// been **built but not merged**. wasm-bindgen mirror of UniFFI's
/// `StagedSweep`.
///
/// Carries byte-for-byte what [`ProposalCommit`] carries, and is a **separate
/// class on purpose**: the payloads are identical but the post-conditions are
/// opposite. Holding a `ProposalCommit` means the local epoch has **already**
/// advanced; holding a `StagedSweep` means it has **not**, and that this device
/// owes the core a verdict. Making that visible in the type means a staged
/// sweep cannot be handed to eager-path code — or the reverse — without a
/// deliberate, reviewer-visible conversion.
///
/// The obligation attached to this value:
///
/// 1. durably record `commit` (and `welcome`) — ADR-0040's outbox;
/// 2. publish them to the group journal;
/// 3. read the journal order back;
/// 4. **won** → [`Group::merge_staged_sweep`]; **lost** →
///    [`Group::abandon_staged_sweep`].
///
/// Steps 3 and 4 are not optional. A staged sweep is persisted by OpenMLS, so
/// leaving one unadjudicated freezes this group's own membership operations
/// **across restarts** — see [`Group::has_staged_sweep`], which is how a
/// freshly loaded group discovers it owes a verdict.
#[wasm_bindgen]
#[derive(Debug)]
pub struct StagedSweep {
    commit_bytes: Vec<u8>,
    welcome_bytes: Option<Vec<u8>>,
    swept_records: Vec<ProposalRecord>,
}

#[wasm_bindgen]
impl StagedSweep {
    /// Encoded MLS commit message — publish to all existing members. The local
    /// epoch has **not** advanced; it advances only on
    /// `group.mergeStagedSweep(...)`.
    #[wasm_bindgen(getter)]
    pub fn commit(&self) -> Vec<u8> {
        self.commit_bytes.clone()
    }

    /// Encoded MLS welcome message — delivered to the newly added members.
    /// `undefined` unless the sweep carried at least one `add`.
    #[wasm_bindgen(getter)]
    pub fn welcome(&self) -> Option<Vec<u8>> {
        self.welcome_bytes.clone()
    }

    /// The proposals this commit carried, in sweep order, as a flat array of
    /// [`ProposalRecord`] instances (same convention as `Group.members()` —
    /// wasm-bindgen cannot return a `Vec` of a custom struct through a getter,
    /// so this is a method, not a property).
    ///
    /// Read back off the staged commit by the core, so it reports what the
    /// commit really carries rather than what the caller predicted it would.
    #[wasm_bindgen]
    pub fn swept(&self) -> Vec<JsValue> {
        self.swept_records
            .iter()
            .map(|r| JsValue::from(r.clone()))
            .collect()
    }
}

/// What a verdict call did to the sweep staged by
/// [`Group::stage_pending_proposals`] (ADR-0045). Mirror of UniFFI's
/// `StagedSweepOutcome`.
///
/// Named rather than a bare `boolean`, because the third state is the one that
/// matters: [`StagedSweepOutcome::NothingStaged`] is **not** a failure — it is
/// what a verdict reports on a group that never staged anything, and it is what
/// makes both verdict entry points idempotent, so ADR-0040's restart sweep can
/// run them twice without consequence.
///
/// Each entry point produces only two of the three variants:
///
/// | Call | Possible outcomes |
/// |---|---|
/// | `group.mergeStagedSweep(...)` | `Merged`, `NothingStaged` |
/// | `group.abandonStagedSweep(...)` | `Abandoned`, `NothingStaged` |
///
/// **The discriminants deliberately start at 1**, so no variant is JavaScript's
/// falsy `0`. `if (outcome)` is therefore never silently false for a real
/// outcome — the same class of trap `ownCommitterRank` documents for rank `0`.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagedSweepOutcome {
    /// A staged sweep was merged: this device is now at the new epoch — the
    /// same one the peers who applied the published commit reached.
    Merged = 1,
    /// A staged sweep was discarded: this device stayed at the epoch the sweep
    /// was built against, and is free to apply the winner's commit. The
    /// proposals the sweep selected are still pending for the winner to sweep.
    Abandoned = 2,
    /// Nothing was staged, so nothing was decided and no state changed.
    NothingStaged = 3,
}

/// What becomes of the outgoing owner when ownership is transferred (ADR-0022's
/// deferred follow-up, built in ADR-0045). Mirror of UniFFI's `OutgoingOwner` /
/// [`lakko_mls::OutgoingOwner`].
///
/// **This is a required argument, never a default.** The core deliberately
/// forces the caller to state it, because either silent choice is wrong in one
/// direction: silently keeping admin leaves an unwanted administrator behind in
/// a group the outgoing owner meant to hand over and walk away from, and
/// silently dropping to plain member strips them, in one irreversible commit,
/// of every power they had in a group they created.
///
/// **The discriminants deliberately start at 1, and that is what carries the
/// "never a default" property across the JS boundary.** A missing or
/// `undefined` argument reaches the wasm ABI as the integer `0`, which is not a
/// declared variant, so wasm-bindgen's generated `FromWasmAbi` throws
/// `"invalid enum value passed"` rather than silently selecting the first
/// variant. Had `Admin` been `0` — the Rust default — omitting the argument
/// would have silently meant `Admin`. `outgoing_owner_variants_are_never_zero`
/// pins this.
///
/// A UI should still **pre-select** [`OutgoingOwner::Admin`] — not because it
/// preserves a way back (it does not: a role change is owner-only, so a
/// transfer cannot be undone by the person who made it) but because it is the
/// recoverable side of an unrecoverable action.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutgoingOwner {
    /// Keep the outgoing owner in the admin set. The variant to pre-select.
    Admin = 1,
    /// Demote the outgoing owner to a plain member.
    Member = 2,
}

impl From<OutgoingOwner> for lakko_mls::OutgoingOwner {
    fn from(o: OutgoingOwner) -> Self {
        match o {
            OutgoingOwner::Admin => Self::Admin,
            OutgoingOwner::Member => Self::Member,
        }
    }
}

/// Stable machine-readable code for the ADR-0045 "a sweep is already staged"
/// refusal, following the crate's coded-`JsError` convention (`stale:` /
/// `invalid:` in `backup.rs`, `release:` in `release.rs`).
///
/// **Superseded, and retained only because it costs nothing.** Every membership
/// refusal now arrives with `err.refusalReason` — for this one,
/// `"sweepAlreadyStaged"` — which is a property read rather than a prefix
/// match, so new callers should branch on that. The prefix is kept on the
/// message because it is documented behaviour and removing it would be a
/// silent behaviour change for anything that already reads it.
///
/// It is the one refusal on the staged path a caller is expected to **handle**
/// rather than log: it means this device already published a sweep and has not
/// yet told the core whether it won or lost, and the correct response is to
/// reach that verdict, never to retry the stage.
pub const SWEEP_ALREADY_STAGED_CODE: &str = "sweep-already-staged:";

/// A member's role in an ADR-0022 role-enabled group. Mirror of UniFFI's
/// `Role`. The web gates the add / remove / promote affordances on this.
#[wasm_bindgen]
#[derive(Clone, Copy)]
pub enum Role {
    /// The group creator — exactly one, unremovable, the only role that may
    /// change the role table.
    Owner,
    /// Appointed by the owner — may add / remove non-owner members.
    Admin,
    /// May not add or remove anyone.
    Member,
}

// ============================================================================
// Group
// ============================================================================

/// Lakko MLS conversation group. Mirror of UniFFI's `Group`.
#[wasm_bindgen]
pub struct Group {
    inner: Mutex<MlsGroup>,
}

#[wasm_bindgen]
impl Group {
    /// Create a fresh group with `identity` as the founding member.
    /// Mirror of UniFFI's `Group::create`.
    #[wasm_bindgen(constructor)]
    pub fn new(identity: &Identity) -> Result<Group, JsValue> {
        let inner = MlsGroup::create(&identity.inner).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Join an existing group from a wire-format Welcome message.
    #[wasm_bindgen(js_name = joinFromWelcome)]
    pub fn join_from_welcome(welcome: &[u8], identity: &Identity) -> Result<Group, JsValue> {
        let inner = MlsGroup::join_from_welcome(welcome, &identity.inner).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Load a previously persisted MLS group from `identity`'s
    /// provider storage (Phase 1.16e6b). Pair with
    /// [`Identity::restoreStorage`](Identity::restore_storage)
    /// — call `restoreStorage` first to seed the provider, then
    /// `Group.load` for each known group_id.
    ///
    /// Returns `undefined` (`Option<Group> -> None`) when
    /// the storage doesn't carry a row for `group_id` —
    /// distinct from a thrown error so the JS side can
    /// distinguish "no group with that id" from "corrupt
    /// storage".
    #[wasm_bindgen(js_name = load)]
    pub fn load(identity: &Identity, group_id: &[u8]) -> Result<Option<Group>, JsValue> {
        match MlsGroup::load(&identity.inner, group_id).map_err(mls_err)? {
            Some(inner) => Ok(Some(Self {
                inner: Mutex::new(inner),
            })),
            None => Ok(None),
        }
    }

    /// Add a member from their published KeyPackage (TLS wire form).
    #[wasm_bindgen(js_name = addMember)]
    pub fn add_member(
        &self,
        identity: &Identity,
        key_package: &[u8],
    ) -> Result<AddMember, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let MemberAddition { commit, welcome } = g
            .add_member_from_wire(&identity.inner, key_package)
            .map_err(mls_err)?;
        Ok(AddMember {
            commit_bytes: group::encode_message(&commit).map_err(mls_err)?,
            welcome_bytes: group::encode_message(&welcome).map_err(mls_err)?,
        })
    }

    /// Add several members in ONE commit with ONE Welcome (ADR-0050): a
    /// person's devices, or a founder's own other devices, must share a
    /// commit, else every leaf but the last is stranded at a Welcome epoch the
    /// founder advanced past with nobody to broadcast the commit to. The
    /// returned Welcome is delivered byte-identical to each added leaf's
    /// mailbox. `keyPackages` is a JS `Uint8Array[]`.
    #[wasm_bindgen(js_name = addMembers)]
    pub fn add_members(
        &self,
        identity: &Identity,
        key_packages: Vec<js_sys::Uint8Array>,
    ) -> Result<AddMember, JsValue> {
        let owned: Vec<Vec<u8>> = key_packages.into_iter().map(|a| a.to_vec()).collect();
        let borrowed: Vec<&[u8]> = owned.iter().map(Vec::as_slice).collect();
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let MemberAddition { commit, welcome } = g
            .add_members_from_wire(&identity.inner, &borrowed)
            .map_err(mls_err)?;
        Ok(AddMember {
            commit_bytes: group::encode_message(&commit).map_err(mls_err)?,
            welcome_bytes: group::encode_message(&welcome).map_err(mls_err)?,
        })
    }

    /// Approve a retained external-join request — **proposes** adding the
    /// joiner (ADR-0027 G-3, on the ADR-0045 proposal path since S7).
    /// `proposal_ref` is the joiner's signature key from a
    /// `ProcessedMessage.externalJoinSignatureKey` or `pendingExternalJoins`.
    ///
    /// **Returns the wire-encoded Proposal to publish — not a Commit, and no
    /// Welcome.** The epoch does not advance; the Welcome is produced by
    /// whichever device sweeps this into a commit (`commitPendingProposals` /
    /// `stagePendingProposals`, whose `welcome` the sweeping device posts as a
    /// `WelcomeRendezvous` for each added key). That is the whole of S7: two
    /// admins approving two joiners against one epoch used to eagerly merge two
    /// commits and fork the group; now they compose into one sweep.
    ///
    /// The approval goes on the wire as an ordinary admin-authored `Add` — the
    /// admin's own MLS-authenticated signature *is* the approval — so it is
    /// indistinguishable from adding a contact and needs no new authorisation
    /// channel. The joiner's own external proposal is still never stored and
    /// never committed: a stranger with a `/g/` link may only ask.
    ///
    /// Role-gated to owner/admin, locally and again by every peer. A refused
    /// approve emits nothing and leaves the request pending, so it can be
    /// retried. A group with no role table has no committer and is refused —
    /// see `PublisherGroup.approveExternalJoin`, whose return type differs
    /// because its outcome does.
    #[wasm_bindgen(js_name = approveExternalJoin)]
    pub fn approve_external_join(
        &self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<Vec<u8>, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        g.approve_external_join(&identity.inner, proposal_ref)
            .map_err(mls_err)
    }

    /// Reject a retained external-join proposal — discards it locally without
    /// adding the joiner or advancing the epoch (ADR-0027).
    /// `identity` is what the durable copy of the pending set is written
    /// through (the request set rides the MLS storage snapshot, so a decision
    /// survives a restart).
    #[wasm_bindgen(js_name = rejectExternalJoin)]
    pub fn reject_external_join(
        &self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<(), JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        g.reject_external_join(&identity.inner, proposal_ref)
            .map_err(mls_err)?;
        Ok(())
    }

    /// Signature keys of the external-join proposals awaiting a decision
    /// (ADR-0027), flat-concatenated — each is a fixed 32-byte Ed25519 key, so
    /// the caller slices into 32-byte chunks (the role-table FFI convention).
    /// Each chunk is a handle for `approveExternalJoin` / `rejectExternalJoin`.
    #[wasm_bindgen(js_name = pendingExternalJoins)]
    pub fn pending_external_joins(&self) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.pending_external_joins().concat())
    }

    /// The account key behind a pending external-join request (32 bytes),
    /// resolved from the joiner's verified ADR-0050 device binding, or
    /// `undefined`. Use it to name a request from a contact; approving still
    /// goes by the leaf key `pendingExternalJoins` returns.
    #[wasm_bindgen(js_name = pendingExternalJoinAccountKey)]
    pub fn pending_external_join_account_key(
        &self,
        joiner_signature_key: &[u8],
    ) -> Result<Option<Vec<u8>>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.pending_external_join_account_key(joiner_signature_key)
            .map(|k| k.to_vec()))
    }

    /// Remove a member by their leaf index. Returns the wire-encoded
    /// commit to broadcast.
    #[wasm_bindgen(js_name = removeMember)]
    pub fn remove_member(&self, identity: &Identity, leaf_index: u32) -> Result<Vec<u8>, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let commit = g
            .remove_member(&identity.inner, lakko_mls::LeafNodeIndex::new(leaf_index))
            .map_err(mls_err)?;
        group::encode_message(&commit).map_err(mls_err)
    }

    /// Encrypt an application payload for the current epoch.
    #[wasm_bindgen(js_name = createApplicationMessage)]
    pub fn create_application_message(
        &self,
        identity: &Identity,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let msg = g
            .create_application_message(&identity.inner, plaintext)
            .map_err(mls_err)?;
        group::encode_message(&msg).map_err(mls_err)
    }

    /// Process an incoming MLS message.
    #[wasm_bindgen(js_name = processIncoming)]
    pub fn process_incoming(
        &self,
        identity: &Identity,
        bytes: &[u8],
    ) -> Result<ProcessedMessage, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let result = g
            .process_incoming(&identity.inner, bytes)
            .map_err(mls_err)?;
        Ok(match result {
            MlsIncoming::Application {
                bytes,
                sender_leaf,
                sender_account,
            } => ProcessedMessage::application(bytes, sender_leaf, sender_account),
            MlsIncoming::Commit => ProcessedMessage::control(ProcessedMessageKind::Commit),
            MlsIncoming::Proposal(record) => ProcessedMessage::from_proposal(record),
            MlsIncoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::external_join(joiner_signature_key, retained),
        })
    }

    /// This member's ADR-0022 role, or `undefined` for an open / legacy group
    /// (no role table). The web gates the add / remove / promote UI on this.
    #[wasm_bindgen(js_name = ownRole)]
    pub fn own_role(&self, identity: &Identity) -> Result<Option<Role>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.own_role(&identity.inner).map(|r| match r {
            MlsRole::Owner => Role::Owner,
            MlsRole::Admin => Role::Admin,
            MlsRole::Member => Role::Member,
        }))
    }

    /// The role table's owner signature key (32 bytes), or `undefined` for an
    /// open / legacy group. A non-null return signals the group is role-enabled.
    #[wasm_bindgen(js_name = roleTableOwner)]
    pub fn role_table_owner(&self) -> Result<Option<Vec<u8>>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.group_roles().map(|t| t.owner().to_vec()))
    }

    /// The role table's admin signature keys, concatenated (each 32 bytes);
    /// empty for an open group or one with no admins. The web slices this into
    /// 32-byte chunks.
    #[wasm_bindgen(js_name = roleTableAdmins)]
    pub fn role_table_admins(&self) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.group_roles()
            .map_or_else(Vec::new, |t| t.admins().concat()))
    }

    /// Rewrite the ADR-0022 role table (owner only — enforced on receive).
    /// `owner` is 32 bytes; `admins_flat` is the concatenation of 32-byte admin
    /// keys. Returns the `GroupContextExtensions` commit wire bytes to broadcast.
    #[wasm_bindgen(js_name = setGroupRoles)]
    pub fn set_group_roles(
        &self,
        identity: &Identity,
        owner: &[u8],
        admins_flat: &[u8],
    ) -> Result<Vec<u8>, JsValue> {
        self.set_group_roles_core(identity, owner, admins_flat)
            .map_err(MlsFailure::into_js)
    }

    /// **Propose** adding a member from their published KeyPackage — the
    /// ADR-0045 proposal-path twin of [`Self::add_member`].
    ///
    /// Returns the wire-encoded Proposal to publish to the group journal. The
    /// epoch does **not** advance and there is no Welcome yet: the Welcome is
    /// produced by whichever device sweeps this into a commit
    /// ([`Self::commit_pending_proposals`]). Because a proposal is not
    /// epoch-advancing, two members proposing concurrently compose instead of
    /// forking the loser off the group.
    ///
    /// Role-gated locally to owner/admin, and re-gated by every peer's receipt
    /// gate — a refused proposal is simply never stored.
    #[wasm_bindgen(js_name = proposeAddMember)]
    pub fn propose_add_member(
        &self,
        identity: &Identity,
        key_package: &[u8],
    ) -> Result<Vec<u8>, JsValue> {
        self.propose_add_member_core(identity, key_package)
            .map_err(MlsFailure::into_js)
    }

    /// **Propose** removing the member at `leaf_index` — the ADR-0045
    /// proposal-path twin of [`Self::remove_member`].
    ///
    /// Naming **this device's own** leaf is how a member leaves and needs no
    /// role; any other leaf needs owner or admin, and the owner's leaf is
    /// refused for everyone. Returns the wire-encoded Proposal to publish; the
    /// epoch does not advance.
    #[wasm_bindgen(js_name = proposeRemoveMember)]
    pub fn propose_remove_member(
        &self,
        identity: &Identity,
        leaf_index: u32,
    ) -> Result<Vec<u8>, JsValue> {
        self.propose_remove_member_core(identity, leaf_index)
            .map_err(MlsFailure::into_js)
    }

    /// **Propose** rewriting the ADR-0022 role table — the ADR-0045
    /// proposal-path twin of [`Self::set_group_roles`]. Owner-only.
    ///
    /// `owner` is 32 bytes; `admins_flat` is the concatenation of 32-byte admin
    /// keys. Returns the wire-encoded Proposal to publish; the epoch does not
    /// advance.
    ///
    /// **One role change at a time.** This THROWS while another role change —
    /// including an ownership transfer — is already pending, because RFC 9420
    /// admits at most one `GroupContextExtensions` proposal per commit, so a
    /// second could not be swept. A pending add or remove does **not** block
    /// it; only another role change does. To pre-empt the throw rather than
    /// surface it, call `pendingProposals()` and look for a `RoleChange` — that
    /// is the same question the core asks.
    #[wasm_bindgen(js_name = proposeGroupRoles)]
    pub fn propose_group_roles(
        &self,
        identity: &Identity,
        owner: &[u8],
        admins_flat: &[u8],
    ) -> Result<Vec<u8>, JsValue> {
        self.propose_group_roles_core(identity, owner, admins_flat)
            .map_err(MlsFailure::into_js)
    }

    /// Every ADR-0045 proposal currently in this device's pending set, in the
    /// order OpenMLS holds them, as a flat array of [`ProposalRecord`]
    /// instances (same convention as [`Self::members`] — wasm-bindgen cannot
    /// return a `Vec` of a custom struct through a getter).
    ///
    /// This is the committer's "N changes waiting to apply" surface and the
    /// proposer's "did my request land" surface. Every entry reports
    /// `stored === true` by construction — the receipt gate admits nothing
    /// else, and OpenMLS empties the whole store on every merged commit, so the
    /// set is exactly what a sweep would consider.
    #[wasm_bindgen(js_name = pendingProposals)]
    pub fn pending_proposals(&self) -> Result<Vec<JsValue>, JsValue> {
        Ok(self
            .pending_proposals_core()
            .map_err(MlsFailure::into_js)?
            .into_iter()
            .map(JsValue::from)
            .collect())
    }

    /// This device's ADR-0045 committer rank: `0` for the owner, `k + 1` for
    /// the `k`-th admin in the role table's canonical order, and
    /// **`undefined`** for a plain member or a group with no role table.
    ///
    /// `undefined` and `0` are **not** the same thing and must never be
    /// conflated: `0` is the owner, who acts as committer immediately, while
    /// `undefined` means this device is not in the committer set at all and
    /// must never sweep. Because `0` is falsy in JavaScript, `if (rank)` is
    /// **wrong** in both directions — test `rank !== undefined` explicitly, and
    /// never `??`-default the value.
    #[wasm_bindgen(js_name = ownCommitterRank)]
    pub fn own_committer_rank(&self, identity: &Identity) -> Result<Option<u32>, JsValue> {
        self.own_committer_rank_core(identity)
            .map_err(MlsFailure::into_js)
    }

    /// **Another** member's ADR-0045 committer rank, by its 32-byte Ed25519
    /// signature public key: `0` for the owner, `k + 1` for the `k`-th admin in
    /// the role table's canonical order, and `undefined` for a plain member, a
    /// non-member, a wrong-length key, or a group with no role table.
    ///
    /// The peer-facing twin of [`Self::own_committer_rank`], and the input
    /// `BeaconObservation.rank` needs: a committer beacon carries no identity,
    /// so the receiver resolves its MLS sender leaf to a signature key through
    /// [`Self::members`] and ranks that key here.
    ///
    /// # Rank the key when you take the verdict, not when the beacon arrived
    ///
    /// A rank is a **position** in the role table's canonical order, not a
    /// stable identity: promoting, demoting or removing any admin renumbers
    /// every admin sorting after it. `actingCommitterVerdict` compares each
    /// observation's rank against this device's own, which is necessarily
    /// derived from the current table — so a rank captured at receipt and
    /// compared later is not merely stale, it is a position in a *different*
    /// ordering. Persist the sender's signature key with each observed beacon
    /// and call this at fold time. Never persist a leaf index for this purpose:
    /// OpenMLS reuses a vacated leaf for the next member added, so a stored
    /// leaf can silently come to name someone else.
    ///
    /// As with [`Self::own_committer_rank`], `undefined` and `0` are **not**
    /// the same thing — `0` is the owner — so `if (rank)` is wrong in both
    /// directions. This orders the authorised committers; it does not widen
    /// them.
    ///
    /// # Errors
    ///
    /// `JsError` if the internal group mutex was poisoned.
    #[wasm_bindgen(js_name = committerRank)]
    pub fn committer_rank(&self, signature_key: &[u8]) -> Result<Option<u32>, JsValue> {
        self.committer_rank_core(signature_key)
            .map_err(MlsFailure::into_js)
    }

    /// Sweep the authorised pending proposals into **one** Commit and merge it
    /// immediately (ADR-0045) — the **eager** committer path.
    ///
    /// **A committer that can lose a race must not use this.** It is exactly
    /// [`Self::stage_pending_proposals`] followed at once by
    /// [`Self::merge_staged_sweep`], and merging before the DS journal has
    /// ordered the commit is precisely what strands the loser at an epoch
    /// nobody else reached — silently and permanently (G-38 residual 1). Two
    /// devices can both act as committer during any handover, so unless the
    /// caller can prove it is the only one committing, the staged pair is the
    /// correct entry point. This one is kept, unchanged, for the callers that
    /// have not migrated.
    ///
    /// Returns `undefined` when nothing authorised is pending: deliberately not
    /// an empty commit, which would advance the epoch (and race other
    /// committers) for no effect. Otherwise the local state is already at the
    /// new epoch when this returns, so the caller MUST durably record the
    /// returned artifacts before publishing them (ADR-0040).
    ///
    /// Role-gated to owner/admin, and the sweep re-runs the ADR-0045
    /// authorisation rule over every candidate, so this can never commit an
    /// unauthorised proposal however the pending store was populated.
    #[wasm_bindgen(js_name = commitPendingProposals)]
    pub fn commit_pending_proposals(
        &self,
        identity: &Identity,
    ) -> Result<Option<ProposalCommit>, JsValue> {
        self.commit_pending_proposals_core(identity)
            .map_err(MlsFailure::into_js)
    }

    /// Sweep the authorised pending proposals into **one** Commit and **stop
    /// before merging it** — the ADR-0045 rollback point (G-38 residual 1).
    ///
    /// This is the committer entry point for any device that can lose a race,
    /// which in practice is every device: ADR-0045's designated committer makes
    /// two simultaneous sweeps rare, not impossible, because no liveness signal
    /// can distinguish a dead committer from a slow one. The DS journal already
    /// arbitrates — every member observes its order identically — so the
    /// collision needs no negotiation. What it needs is for the loser to be
    /// able to *survive losing*, which the eager shape denies it.
    ///
    /// **The sequence, in order. Every step is required.**
    ///
    /// 1. `stagePendingProposals` — the group stays at epoch N holding an
    ///    un-merged commit.
    /// 2. Durably record `staged.commit` (and `staged.welcome`) — ADR-0040's
    ///    outbox — then publish them.
    /// 3. Read the journal order back: did this commit land first?
    /// 4. **Won** → `mergeStagedSweep`. **Lost** → `abandonStagedSweep`, then
    ///    apply the winner's commit normally.
    ///
    /// The two mechanisms answer different questions and neither subsumes the
    /// other: the outbox answers *did my commit reach the wire*, this pair
    /// answers *did it win the wire order*. The first must be `yes` before the
    /// second is meaningful. A commit that **won** must be merged and never
    /// rolled back — rolling one back after peers applied it manufactures the
    /// very fork this closes.
    ///
    /// **Abandoning costs nothing.** OpenMLS empties the proposal store on
    /// *merge*, and an abandoned sweep was never merged, so the proposals it
    /// selected are still pending for the winner's next sweep.
    ///
    /// Returns `undefined` when nothing authorised is pending — the same
    /// deliberate non-empty-commit rule as [`Self::commit_pending_proposals`],
    /// and in that case nothing is staged and no verdict is owed.
    ///
    /// Throws a [`SWEEP_ALREADY_STAGED_CODE`]-coded `JsError` when a sweep is
    /// already awaiting its verdict — handle it by reaching that verdict, never
    /// by retrying the stage. Every other failure (no role table, this device
    /// not owner or admin, an OpenMLS-side error) throws an uncoded `JsError`.
    #[wasm_bindgen(js_name = stagePendingProposals)]
    pub fn stage_pending_proposals(
        &self,
        identity: &Identity,
    ) -> Result<Option<StagedSweep>, JsValue> {
        self.stage_pending_proposals_core(identity)
            .map_err(MlsFailure::into_js)
    }

    /// `true` when a sweep built by [`Self::stage_pending_proposals`] is staged
    /// and still awaiting its journal verdict (ADR-0045).
    ///
    /// **This is the restart-time obligation check, and it is why the verdict
    /// lives on the group rather than on the staged value.** A staged commit is
    /// persisted by OpenMLS and read back by `Group.load`, so it outlives the
    /// page — a JS object returned by the stage call could not. After loading a
    /// group, a client that finds this `true` still owes a verdict on a commit
    /// it may have already published, and must resolve it (ADR-0040's restart
    /// sweep re-publishes, re-reads the journal order, then merges or
    /// abandons).
    ///
    /// Leaving one unresolved is not inert: while a sweep is staged, OpenMLS
    /// refuses this group's own `propose*` / add / remove / role-change
    /// operations, so the group's membership is frozen until the verdict lands.
    /// Incoming messages are unaffected.
    ///
    /// Every eager path leaves this `false` — [`Self::add_member`],
    /// [`Self::remove_member`], [`Self::set_group_roles`] and
    /// [`Self::commit_pending_proposals`] all merge before they return — which
    /// is why a rollback here can never reach a path that never had one.
    /// [`Self::approve_external_join`] left that list with ADR-0045 S7: it
    /// merges nothing, because it commits nothing.
    #[wasm_bindgen(js_name = hasStagedSweep)]
    pub fn has_staged_sweep(&self) -> Result<bool, JsValue> {
        self.has_staged_sweep_core().map_err(MlsFailure::into_js)
    }

    /// **Won.** Merge the sweep staged by [`Self::stage_pending_proposals`],
    /// advancing this device to the new epoch — the same one the peers who
    /// applied the published commit reached (ADR-0045).
    ///
    /// Call this only after the journal has confirmed this device's commit was
    /// ordered first. Merging on any weaker signal is the eager shape again,
    /// with all of its consequences.
    ///
    /// Returns [`StagedSweepOutcome::Merged`], or
    /// [`StagedSweepOutcome::NothingStaged`] when there was nothing staged —
    /// not a failure, and what makes a re-run after the merge a no-op.
    #[wasm_bindgen(js_name = mergeStagedSweep)]
    pub fn merge_staged_sweep(&self, identity: &Identity) -> Result<StagedSweepOutcome, JsValue> {
        self.merge_staged_sweep_core(identity)
            .map_err(MlsFailure::into_js)
    }

    /// **Lost.** Discard the sweep staged by [`Self::stage_pending_proposals`],
    /// returning this group to the epoch the sweep was built against so it can
    /// apply the winner's commit and carry on (ADR-0045).
    ///
    /// This touches one group and no other: the rollback sets **this** group's
    /// state back to operational and writes nothing else, unlike a
    /// whole-provider `restoreStorage`, which would roll sibling groups back
    /// past commits they have already published.
    ///
    /// Returns [`StagedSweepOutcome::Abandoned`], or
    /// [`StagedSweepOutcome::NothingStaged`] when there was nothing staged —
    /// which is what every eagerly-merged path reports, and is why calling this
    /// after one cannot undo anything.
    ///
    /// Throws when the discard did not take effect: the core re-reads the group
    /// state afterwards and refuses to report a rollback it did not actually
    /// perform (fail-closed, invariant #4).
    #[wasm_bindgen(js_name = abandonStagedSweep)]
    pub fn abandon_staged_sweep(&self, identity: &Identity) -> Result<StagedSweepOutcome, JsValue> {
        self.abandon_staged_sweep_core(identity)
            .map_err(MlsFailure::into_js)
    }

    /// Hand the group's owner slot to `new_owner`, as a **direct commit**
    /// (ADR-0022's deferred ownership-transfer follow-up, built in ADR-0045).
    ///
    /// `new_owner` is the member's 32-byte Ed25519 signature public key — the
    /// same stable handle `Group.members()` reports and the role table keys on,
    /// never a leaf index. `outgoing` decides what becomes of **this** device's
    /// own role and is **required**: omitting it throws rather than defaulting
    /// (see [`OutgoingOwner`]).
    ///
    /// **Owner-only and irreversible.** Only the owner may transfer, because a
    /// role change is owner-only and a transfer is a role change; anything
    /// weaker would be a coup, since an admin able to transfer could name
    /// itself. Once the commit lands, only the new owner can rewrite the table.
    /// This does **not** recover a *lost* owner either — minting the commit
    /// needs the owner's key.
    ///
    /// Transfer is what makes leaving possible: an owner may not leave while
    /// owner, but the former owner is an ordinary member and may. The two are
    /// necessarily separate commits — both proposals are judged against the
    /// pre-merge table in which the transferor is still owner, so a fused
    /// transfer-and-leave has its `Leave` half refused.
    ///
    /// Transfer is deliberately **not** a new commit type: it is the ordinary
    /// owner-only role-table rewrite with the owner slot moved, so it rides the
    /// audited [`Self::set_group_roles`] path and is judged by the existing
    /// peer gate. Returns the wire-encoded commit to broadcast; the local epoch
    /// has already advanced, so record it durably before publishing
    /// (ADR-0040). In a group that has moved to the ADR-0045 proposal path,
    /// prefer [`Self::propose_ownership_transfer`].
    ///
    /// Throws when `new_owner` is not 32 bytes, when the group carries no role
    /// table, when this device is not the owner, when `new_owner` is already
    /// the owner, or when `new_owner` is **not a member of this group** — the
    /// last refused because an owner outside the group can never be replaced (a
    /// role change is owner-only) and never removed (the owner's leaf is
    /// unremovable), leaving the group permanently un-administrable. Peers
    /// refuse such a commit independently (C-63), so this local refusal is the
    /// clear error, not the enforcement.
    #[wasm_bindgen(js_name = transferOwnership)]
    pub fn transfer_ownership(
        &self,
        identity: &Identity,
        new_owner: &[u8],
        outgoing: OutgoingOwner,
    ) -> Result<Vec<u8>, JsValue> {
        self.transfer_ownership_core(identity, new_owner, outgoing)
            .map_err(MlsFailure::into_js)
    }

    /// **Propose** handing the group's owner slot to `new_owner` (ADR-0045) —
    /// the proposal-path twin of [`Self::transfer_ownership`], with identical
    /// authorisation and identical refusals.
    ///
    /// The proposal is judged, by this device and by every peer, against the
    /// **pre-merge** table in which this device is still the owner, so it needs
    /// no new rule: it is a role change, which is owner-only. Any owner-or-admin
    /// may sweep it, including the incoming owner. Returns the wire-encoded
    /// Proposal to publish; the epoch does **not** advance.
    #[wasm_bindgen(js_name = proposeOwnershipTransfer)]
    pub fn propose_ownership_transfer(
        &self,
        identity: &Identity,
        new_owner: &[u8],
        outgoing: OutgoingOwner,
    ) -> Result<Vec<u8>, JsValue> {
        self.propose_ownership_transfer_core(identity, new_owner, outgoing)
            .map_err(MlsFailure::into_js)
    }

    /// Process one row read from this journal's **knock** queue (ADR-0053).
    /// Anything that is not a stranger's external-join proposal for this group
    /// is refused BEFORE it is processed - a knock row is an unauthenticated
    /// write, and applying a commit or message someone posted there instead of
    /// the journal would fork the admins who read knocks from everyone else.
    /// A row that passes is processed exactly like `processIncomingVerifyingRoles`.
    #[wasm_bindgen(js_name = processKnock)]
    pub fn process_knock(
        &self,
        identity: &Identity,
        bytes: &[u8],
    ) -> Result<ProcessedMessage, JsValue> {
        {
            let g = self
                .inner
                .lock()
                .map_err(|_| js_error("internal lock poisoned"))?;
            if !lakko_mls::is_external_join_proposal_for(bytes, &g.group_id()) {
                return Err(js_error(
                    "knock row is not an external-join proposal for this group",
                ));
            }
        }
        self.process_incoming_verifying_roles(identity, bytes)
    }

    /// Apply an incoming commit ENFORCING the ADR-0022 role policy — a commit
    /// whose committer's role does not authorise its effect is REFUSED (not
    /// merged), fail-closed. A group with no role table behaves like
    /// [`Self::process_incoming`]. Use on the chat-message receive loop only;
    /// do NOT use for call-group receives (ADR-0022 call-path caveat).
    #[wasm_bindgen(js_name = processIncomingVerifyingRoles)]
    pub fn process_incoming_verifying_roles(
        &self,
        identity: &Identity,
        bytes: &[u8],
    ) -> Result<ProcessedMessage, JsValue> {
        self.process_incoming_verifying_roles_core(identity, bytes)
            .map_err(MlsFailure::into_js)
    }

    /// The ADR-0053 access signer for this journal at the current epoch:
    /// `export_secret("lakko/ds/access/v1", groupId, 32)` wrapped as a signer
    /// whose `sign` produces the `x-lakko-ds-proof` header a claimed journal
    /// requires and whose `groupClaimBodyJson` registers the epoch's key.
    /// Re-derive after every commit.
    #[wasm_bindgen(js_name = dsAccessSigner)]
    pub fn ds_access_signer(
        &self,
        identity: &Identity,
    ) -> Result<crate::ds_proof::DsProofSigner, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        crate::ds_proof::group_signer(&g, &identity.inner)
    }

    /// The ADR-0053 access signer for the epoch a staged sweep moves to
    /// (N+1), while this group still sits at N; `undefined` when nothing is
    /// staged. Register its key, proven under the current signer, BEFORE
    /// publishing the sweep.
    #[wasm_bindgen(js_name = dsStagedAccessSigner)]
    pub fn ds_staged_access_signer(
        &self,
        identity: &Identity,
    ) -> Result<Option<crate::ds_proof::DsProofSigner>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        crate::ds_proof::staged_group_signer(&g, &identity.inner)
    }

    /// Derive a `length`-byte secret bound to the current epoch.
    #[wasm_bindgen(js_name = exportSecret)]
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: &str,
        context: &[u8],
        length: u32,
    ) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let secret = g
            .export_secret(&identity.inner, label, context, length as usize)
            .map_err(mls_err)?;
        Ok(secret.to_vec())
    }

    /// Whether this device is still a member of the group.
    ///
    /// `false` means a Commit removing this device's own leaf has been applied.
    /// The state is **terminal** — RFC 9420 offers no way back, and it survives
    /// a snapshot / restore, so this answers the same at launch as it did in
    /// the poll batch that carried the removal. Rejoining is a new `Group`.
    ///
    /// Nothing in the receive path signals the transition:
    /// `processIncomingVerifyingRoles` returns the same content-less `Commit`
    /// for a removing commit as for any other. **This is the only signal.**
    ///
    /// Once it is `false`, reads still answer (`members`, `epoch`,
    /// `memberCount`, `ownLeafIndex`, `groupId`, `groupRoles`) but are frozen
    /// at the removing epoch and will never move again, while
    /// `createApplicationMessage`, `exportSecret` and every further decrypt
    /// throw — including for rows still sitting unfetched on the DS, since the
    /// refusal is not epoch-scoped.
    ///
    /// The UI gates on it: withdraw the composer and the membership
    /// affordances, stop polling and stop emitting beacons, and keep rendering
    /// the history, which is still readable. Prefer this over inferring
    /// membership from `members()` — the inference rests on a leaf signature
    /// key being the account key, which ADR-0022's per-device-leaf follow-up
    /// would break silently.
    #[wasm_bindgen(getter, js_name = isActive)]
    pub fn is_active(&self) -> Result<bool, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.is_active())
    }

    /// Local leaf index of the calling identity.
    #[wasm_bindgen(getter, js_name = ownLeafIndex)]
    pub fn own_leaf_index(&self) -> Result<u32, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.own_leaf_index().u32())
    }

    /// Current group epoch.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> Result<u64, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.epoch())
    }

    /// Number of members at the current epoch.
    #[wasm_bindgen(getter, js_name = memberCount)]
    pub fn member_count(&self) -> Result<u32, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(u32::try_from(g.member_count()).unwrap_or(u32::MAX))
    }

    /// MLS `group_id` (RFC 9420 §6.2). Used by `apps/web` as
    /// the routing key for `POST /ds/group/{group_id_hex}`. The
    /// bytes are opaque — 32 random bytes minted by
    /// `Group::create` (`lakko_mls::GROUP_ID_LEN`), which is the
    /// width the DS requires of that path segment. Note this is
    /// *not* OpenMLS's own default: `GroupId::random` is 16
    /// bytes, so `Group::create` mints the id itself. Identical
    /// for every member of the group at every epoch.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.group_id())
    }

    /// Snapshot of every member's `(leafIndex, signaturePublicKey)`
    /// at the current epoch (Phase 1.18h).
    ///
    /// JS-side shape: returns a flat array of [`GroupMember`]
    /// instances. wasm-bindgen doesn't let us return `Vec<JsValue>`
    /// of a custom struct directly through a getter, so this is
    /// a regular method (`.members()` not `.members`) returning
    /// `Vec<JsValue>` after a per-element `JsValue::from`.
    ///
    /// Ordering matches `lakko_mls::Group::members` — left-to-right
    /// over the leaf tree, stable inside a single epoch.
    #[wasm_bindgen]
    pub fn members(&self) -> Result<Vec<JsValue>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.members()
            .into_iter()
            .map(|m| {
                JsValue::from(GroupMember {
                    leaf_index_val: m.leaf_index,
                    signature_public_key_val: m.signature_public_key,
                    account: m.account_key,
                })
            })
            .collect())
    }
}

/// Host-testable cores of the ADR-0045 proposal surface.
///
/// `JsValue` can only be constructed on the `wasm32` target, so every exported
/// method above is unreachable from `cargo test`. Keeping the behaviour here —
/// in plain Rust, erroring with an [`MlsFailure`] the wrapper turns into the
/// thrown `Error` — is what lets the host suite cover ADR-0045 end to end
/// instead of only its happy path.
///
/// [`MlsFailure`] rather than a bare `String` is what keeps a membership
/// refusal's typed reason alive down here, where the assertions are: flattening
/// to text at this layer would mean the only thing the host suite could check
/// about a refusal is its prose.
impl Group {
    /// Lock the inner group, reporting poisoning as a plain message.
    fn locked(&self) -> Result<std::sync::MutexGuard<'_, MlsGroup>, MlsFailure> {
        self.inner
            .lock()
            .map_err(|_| "internal lock poisoned".into())
    }

    /// Core of [`Self::set_group_roles`].
    fn set_group_roles_core(
        &self,
        identity: &Identity,
        owner: &[u8],
        admins_flat: &[u8],
    ) -> Result<Vec<u8>, MlsFailure> {
        let table = role_table_from_flat(owner, admins_flat).map_err(MlsFailure::from)?;
        self.locked()?
            .set_group_roles(&identity.inner, &table)
            .map_err(MlsFailure::from)
    }

    /// Core of [`Self::process_incoming_verifying_roles`].
    fn process_incoming_verifying_roles_core(
        &self,
        identity: &Identity,
        bytes: &[u8],
    ) -> Result<ProcessedMessage, MlsFailure> {
        let result = self
            .locked()?
            .process_incoming_verifying_roles(&identity.inner, bytes)
            .map_err(MlsFailure::from)?;
        Ok(match result {
            MlsIncoming::Application {
                bytes,
                sender_leaf,
                sender_account,
            } => ProcessedMessage::application(bytes, sender_leaf, sender_account),
            MlsIncoming::Commit => ProcessedMessage::control(ProcessedMessageKind::Commit),
            MlsIncoming::Proposal(record) => ProcessedMessage::from_proposal(record),
            MlsIncoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::external_join(joiner_signature_key, retained),
        })
    }

    /// Core of [`Self::propose_add_member`].
    fn propose_add_member_core(
        &self,
        identity: &Identity,
        key_package: &[u8],
    ) -> Result<Vec<u8>, MlsFailure> {
        self.locked()?
            .propose_add_member_from_wire(&identity.inner, key_package)
            .map_err(MlsFailure::from)
    }

    /// Core of [`Self::propose_remove_member`].
    fn propose_remove_member_core(
        &self,
        identity: &Identity,
        leaf_index: u32,
    ) -> Result<Vec<u8>, MlsFailure> {
        self.locked()?
            .propose_remove_member(&identity.inner, lakko_mls::LeafNodeIndex::new(leaf_index))
            .map_err(MlsFailure::from)
    }

    /// Core of [`Self::propose_group_roles`].
    fn propose_group_roles_core(
        &self,
        identity: &Identity,
        owner: &[u8],
        admins_flat: &[u8],
    ) -> Result<Vec<u8>, MlsFailure> {
        let table = role_table_from_flat(owner, admins_flat).map_err(MlsFailure::from)?;
        self.locked()?
            .propose_group_roles(&identity.inner, &table)
            .map_err(MlsFailure::from)
    }

    /// Core of [`Self::pending_proposals`].
    fn pending_proposals_core(&self) -> Result<Vec<ProposalRecord>, MlsFailure> {
        Ok(self
            .locked()?
            .pending_proposals()
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Core of [`Self::own_committer_rank`].
    fn own_committer_rank_core(&self, identity: &Identity) -> Result<Option<u32>, MlsFailure> {
        Ok(self.locked()?.own_committer_rank(&identity.inner))
    }

    /// Core of [`Self::committer_rank`].
    fn committer_rank_core(&self, signature_key: &[u8]) -> Result<Option<u32>, MlsFailure> {
        Ok(self.locked()?.committer_rank_of(signature_key))
    }

    /// Core of [`Self::commit_pending_proposals`].
    fn commit_pending_proposals_core(
        &self,
        identity: &Identity,
    ) -> Result<Option<ProposalCommit>, MlsFailure> {
        let Some(swept_commit) = self
            .locked()?
            .commit_pending_proposals(&identity.inner)
            .map_err(MlsFailure::from)?
        else {
            return Ok(None);
        };
        let encode = |m: &_| group::encode_message(m).map_err(MlsFailure::from);
        let welcome_bytes = match swept_commit.welcome.as_ref() {
            Some(w) => Some(encode(w)?),
            None => None,
        };
        Ok(Some(ProposalCommit {
            commit_bytes: encode(&swept_commit.commit)?,
            welcome_bytes,
            swept_records: swept_commit.swept.into_iter().map(Into::into).collect(),
        }))
    }

    /// Core of [`Self::stage_pending_proposals`].
    fn stage_pending_proposals_core(
        &self,
        identity: &Identity,
    ) -> Result<Option<StagedSweep>, MlsFailure> {
        let mut g = self.locked()?;
        // Projected here, under the SAME lock guard the stage runs under, so
        // there is no window between the check and the call. The core carries
        // its own fail-closed guard and stays authoritative; this exists only
        // to give the caller a stable code it can branch on instead of prose it
        // would have to string-match, and never to relax the refusal.
        if g.has_staged_sweep() {
            return Err(MlsFailure {
                message: format!(
                    "{SWEEP_ALREADY_STAGED_CODE} sweep refused: a staged sweep is already \
                     awaiting a verdict"
                ),
                reason: Some(MlsRefusalReason::SweepAlreadyStaged),
            });
        }
        let Some(staged) = g
            .stage_pending_proposals(&identity.inner)
            .map_err(MlsFailure::from)?
        else {
            return Ok(None);
        };
        let encode = |m: &_| group::encode_message(m).map_err(MlsFailure::from);
        let welcome_bytes = match staged.welcome.as_ref() {
            Some(w) => Some(encode(w)?),
            None => None,
        };
        Ok(Some(StagedSweep {
            commit_bytes: encode(&staged.commit)?,
            welcome_bytes,
            swept_records: staged.swept.into_iter().map(Into::into).collect(),
        }))
    }

    /// Core of [`Self::has_staged_sweep`].
    fn has_staged_sweep_core(&self) -> Result<bool, MlsFailure> {
        Ok(self.locked()?.has_staged_sweep())
    }

    /// Core of [`Self::merge_staged_sweep`].
    fn merge_staged_sweep_core(
        &self,
        identity: &Identity,
    ) -> Result<StagedSweepOutcome, MlsFailure> {
        let merged = self
            .locked()?
            .merge_staged_sweep(&identity.inner)
            .map_err(MlsFailure::from)?;
        Ok(if merged {
            StagedSweepOutcome::Merged
        } else {
            StagedSweepOutcome::NothingStaged
        })
    }

    /// Core of [`Self::abandon_staged_sweep`].
    fn abandon_staged_sweep_core(
        &self,
        identity: &Identity,
    ) -> Result<StagedSweepOutcome, MlsFailure> {
        let abandoned = self
            .locked()?
            .abandon_staged_sweep(&identity.inner)
            .map_err(MlsFailure::from)?;
        Ok(if abandoned {
            StagedSweepOutcome::Abandoned
        } else {
            StagedSweepOutcome::NothingStaged
        })
    }

    /// Core of [`Self::transfer_ownership`].
    fn transfer_ownership_core(
        &self,
        identity: &Identity,
        new_owner: &[u8],
        outgoing: OutgoingOwner,
    ) -> Result<Vec<u8>, MlsFailure> {
        if new_owner.len() != 32 {
            return Err("new owner key must be 32 bytes".into());
        }
        self.locked()?
            .transfer_ownership(&identity.inner, new_owner, outgoing.into())
            .map_err(MlsFailure::from)
    }

    /// Core of [`Self::propose_ownership_transfer`].
    fn propose_ownership_transfer_core(
        &self,
        identity: &Identity,
        new_owner: &[u8],
        outgoing: OutgoingOwner,
    ) -> Result<Vec<u8>, MlsFailure> {
        if new_owner.len() != 32 {
            return Err("new owner key must be 32 bytes".into());
        }
        self.locked()?
            .propose_ownership_transfer(&identity.inner, new_owner, outgoing.into())
            .map_err(MlsFailure::from)
    }
}

// ============================================================================
// PublisherGroup (G4-2-mls-persistence step 2 — wasm-bindgen mirror of the
// UniFFI surface. Same load-or-create dance for the Phase 4 channels Tier-1
// MLS group; web composers go through this instead of the throwaway-group
// path the prior implementation used.)
// ============================================================================

/// Persistent MLS-group wrapper for one Lakko channel's
/// publisher tier (ADR-0010 Tier 1). Mirror of the
/// UniFFI `PublisherGroup` (`crates/lakko-ffi-uniffi`).
#[wasm_bindgen]
pub struct PublisherGroup {
    inner: Mutex<MlsPublisherGroup>,
}

#[wasm_bindgen]
impl PublisherGroup {
    /// Mint a fresh PublisherGroup for `channel_id`. Persists
    /// the MLS state via the OpenMLS provider; subsequent
    /// [`PublisherGroup.load`] calls return the same group.
    ///
    /// `channel_id` MUST be exactly 16 bytes (
    /// [`lakko_mls::CHANNEL_ID_LEN`]).
    #[wasm_bindgen(constructor)]
    pub fn new(identity: &Identity, channel_id: &[u8]) -> Result<PublisherGroup, JsValue> {
        let cid = MlsChannelId::try_from_slice(channel_id).map_err(mls_err)?;
        let inner = MlsPublisherGroup::create(&identity.inner, &cid).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Load the persisted PublisherGroup for `channel_id`.
    /// Returns `undefined` (`Option<...> -> None`) when no
    /// group has been minted yet — pair with
    /// [`PublisherGroup.openOrCreate`] for the load-or-create
    /// path every composer wants.
    #[wasm_bindgen(js_name = load)]
    pub fn load(identity: &Identity, channel_id: &[u8]) -> Result<Option<PublisherGroup>, JsValue> {
        let cid = MlsChannelId::try_from_slice(channel_id).map_err(mls_err)?;
        match MlsPublisherGroup::load(&identity.inner, &cid).map_err(mls_err)? {
            Some(inner) => Ok(Some(Self {
                inner: Mutex::new(inner),
            })),
            None => Ok(None),
        }
    }

    /// Load-or-create convenience. Returns the persisted
    /// PublisherGroup for `(identity, channel_id)` if one
    /// exists; otherwise mints a fresh one and persists it
    /// as a side effect. Idempotent across consecutive
    /// calls — the load-bearing invariant that lets
    /// subscribers' previously-received seed envelopes keep
    /// decrypting across publisher restarts.
    #[wasm_bindgen(js_name = openOrCreate)]
    pub fn open_or_create(
        identity: &Identity,
        channel_id: &[u8],
    ) -> Result<PublisherGroup, JsValue> {
        if let Some(existing) = Self::load(identity, channel_id)? {
            return Ok(existing);
        }
        Self::new(identity, channel_id)
    }

    /// 32-byte channel content-key seed from the MLS
    /// exporter at the current group epoch. Stable across
    /// process restarts once the group has been persisted —
    /// hand directly to the channels composer's
    /// `ChannelPublisherKeys.seed` field.
    #[wasm_bindgen(js_name = channelSeed)]
    pub fn channel_seed(&self, identity: &Identity) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let seed = g.channel_seed(&identity.inner).map_err(mls_err)?;
        Ok(seed.to_vec())
    }

    /// Channel id this group was minted for. Returned as raw
    /// bytes (16 elements) so the JS side can compare with
    /// whatever shape the channel-discovery layer hands it.
    #[wasm_bindgen(js_name = channelId)]
    pub fn channel_id(&self) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.channel_id().as_bytes().to_vec())
    }

    /// G4-2-mls-persistence step 4 — rotate the channel
    /// content-key seed via an MLS self-update commit.
    /// Mirror of the UniFFI `PublisherGroup.rotate`. Returns
    /// the wire-encoded commit bytes; the caller broadcasts
    /// them to every other PublisherGroup member so they
    /// advance to the same epoch in lock-step.
    ///
    /// After `rotate` resolves, the next `channelSeed` call
    /// returns the **new-epoch** seed. The web composer
    /// pairs this with `sealSubscriberSeedEnvelope` + the
    /// services/channels batch fanout to reseal the seed to
    /// every subscriber under the new epoch id.
    #[wasm_bindgen(js_name = rotate)]
    pub fn rotate(&self, identity: &Identity) -> Result<Vec<u8>, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        g.rotate(&identity.inner).map_err(mls_err)
    }

    // ADR-0028 OQ3 — co-publisher external-join surface. A PublisherGroup is a
    // vanilla MLS group, so adding a publisher reuses the group join-ticket +
    // external-join machinery (ADR-0027/0028) verbatim. These delegate to the
    // inner Group; the owner mints a `/g/` link over `groupId`, applies the
    // joiner's external-add via `processIncomingVerifyingRoles`, and approves.

    /// The ADR-0053 access signer for the publisher journal at the current
    /// epoch - the same derivation as `Group.dsAccessSigner` over the
    /// underlying group. Re-derive after every commit (every rotation).
    #[wasm_bindgen(js_name = dsAccessSigner)]
    pub fn ds_access_signer(
        &self,
        identity: &Identity,
    ) -> Result<crate::ds_proof::DsProofSigner, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        crate::ds_proof::group_signer(g.inner(), &identity.inner)
    }

    /// ADR-0028 OQ3 — the underlying MLS `group_id`. A co-publisher invite-link
    /// is minted over this; the joiner targets it when crafting its
    /// external-join proposal.
    #[wasm_bindgen(js_name = groupId)]
    pub fn group_id(&self) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.group_id())
    }

    /// Process one row read from this journal's **knock** queue (ADR-0053).
    /// Anything that is not a stranger's external-join proposal for this group
    /// is refused BEFORE it is processed - a knock row is an unauthenticated
    /// write, and applying a commit or message someone posted there instead of
    /// the journal would fork the admins who read knocks from everyone else.
    /// A row that passes is processed exactly like `processIncomingVerifyingRoles`.
    #[wasm_bindgen(js_name = processKnock)]
    pub fn process_knock(
        &self,
        identity: &Identity,
        bytes: &[u8],
    ) -> Result<ProcessedMessage, JsValue> {
        {
            let g = self
                .inner
                .lock()
                .map_err(|_| js_error("internal lock poisoned"))?;
            if !lakko_mls::is_external_join_proposal_for(bytes, &g.group_id()) {
                return Err(js_error(
                    "knock row is not an external-join proposal for this group",
                ));
            }
        }
        self.process_incoming_verifying_roles(identity, bytes)
    }

    /// ADR-0028 OQ3 — process an incoming MLS message on the publisher tier,
    /// role-gated (ADR-0022). Surfaces a non-member's external-join proposal as
    /// `ProcessedMessage.externalJoinSignatureKey` so the owner can approve a
    /// co-publisher.
    #[wasm_bindgen(js_name = processIncomingVerifyingRoles)]
    pub fn process_incoming_verifying_roles(
        &self,
        identity: &Identity,
        bytes: &[u8],
    ) -> Result<ProcessedMessage, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let result = g
            .inner_mut()
            .process_incoming_verifying_roles(&identity.inner, bytes)
            .map_err(mls_err)?;
        Ok(match result {
            MlsIncoming::Application {
                bytes,
                sender_leaf,
                sender_account,
            } => ProcessedMessage::application(bytes, sender_leaf, sender_account),
            MlsIncoming::Commit => ProcessedMessage::control(ProcessedMessageKind::Commit),
            MlsIncoming::Proposal(record) => ProcessedMessage::from_proposal(record),
            MlsIncoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::external_join(joiner_signature_key, retained),
        })
    }

    /// ADR-0028 OQ3 — signature keys of the pending co-publisher external-join
    /// proposals, flat-concatenated 32-byte chunks (the role-table FFI
    /// convention). Each chunk is a handle for `approveExternalJoin` /
    /// `rejectExternalJoin`.
    #[wasm_bindgen(js_name = pendingExternalJoins)]
    pub fn pending_external_joins(&self) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.inner().pending_external_joins().concat())
    }

    /// ADR-0028 OQ3 — approve a retained co-publisher external-join; returns
    /// the Commit + the joiner's Welcome, as it always has.
    ///
    /// **This is the direct approve, and this is the only type it survives on**
    /// (ADR-0045 S7). A PublisherGroup carries no ADR-0022 role table, so it has
    /// no committer to sweep a proposal, and ADR-0045 leaves such a group on
    /// direct commits. The return type is the difference and it is deliberate:
    /// `Group.approveExternalJoin` hands back proposal bytes and no Welcome,
    /// this hands back a merged Commit and a Welcome to deliver now, and neither
    /// can be used where the other is expected.
    #[wasm_bindgen(js_name = approveExternalJoin)]
    pub fn approve_external_join(
        &self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<AddMember, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let MemberAddition { commit, welcome } = g
            .approve_external_join(&identity.inner, proposal_ref)
            .map_err(mls_err)?;
        Ok(AddMember {
            commit_bytes: group::encode_message(&commit).map_err(mls_err)?,
            welcome_bytes: group::encode_message(&welcome).map_err(mls_err)?,
        })
    }

    /// ADR-0028 OQ3 — reject a retained co-publisher external-join (local drop;
    /// no epoch advance).
    /// `identity` is what the durable copy of the pending set is written
    /// through (the request set rides the MLS storage snapshot, so a decision
    /// survives a restart).
    #[wasm_bindgen(js_name = rejectExternalJoin)]
    pub fn reject_external_join(
        &self,
        identity: &Identity,
        proposal_ref: &[u8],
    ) -> Result<(), JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        g.inner_mut()
            .reject_external_join(&identity.inner, proposal_ref)
            .map_err(mls_err)?;
        Ok(())
    }
}

/// ADR-0028 OQ3 — recover the 16-byte channel id from a PublisherGroup
/// `group_id` (the inverse of the deterministic derivation). Returns
/// `undefined` for bytes that do not carry the publisher-group prefix. A
/// co-publisher uses this after joining via a `/g/` ticket — which conveys only
/// the `group_id` — to reconstruct the channel + derive its content-key seed.
#[wasm_bindgen(js_name = channelIdFromPublisherGroupId)]
pub fn channel_id_from_publisher_group_id(group_id: &[u8]) -> Option<Vec<u8>> {
    lakko_mls::channel_id_from_group_id(group_id).map(|c| c.as_bytes().to_vec())
}

// ============================================================================
// DeviceGroup (Phase 2a3b1d)
// ============================================================================

/// Per-user multi-device MLS group (ADR-0007). Mirror of
/// UniFFI's `DeviceGroup`.
///
/// FFI subset for 2a3b's add-device flow:
///
///   * `createFounder` — bootstrap on this device.
///   * `joinFromWelcome` — join from a Welcome the scanner
///     produced + delivered via DS mailbox.
///   * `addDeviceFromWire` — add a fresh device by its
///     KeyPackage; returns the (commit, welcome) pair.
///   * `groupId` — the deterministic
///     `BLAKE3("lakko-device-group/v1" || account_id)`.
///
/// 2a3c widens this with `devices` + `removeDevice` once the
/// per-platform UI lands.
#[wasm_bindgen]
pub struct DeviceGroup {
    inner: Mutex<MlsDeviceGroup>,
}

#[wasm_bindgen]
impl DeviceGroup {
    /// Bootstrap a fresh DeviceGroup with `identity` as the
    /// founding leaf. `account_id` MUST be the raw 16-byte
    /// UUIDv7.
    #[wasm_bindgen(js_name = createFounder)]
    pub fn create_founder(identity: &Identity, account_id: &[u8]) -> Result<DeviceGroup, JsValue> {
        if account_id.len() != 16 {
            return Err(js_error("account_id must be 16 bytes"));
        }
        let mut acct = [0u8; 16];
        acct.copy_from_slice(account_id);
        let inner = MlsDeviceGroup::create_founder(&identity.inner, &acct).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Join a DeviceGroup from a Welcome the scanner device
    /// produced via [`DeviceGroup::addDeviceFromWire`]. The
    /// deterministic `group_id` check inside
    /// `lakko_mls::DeviceGroup::join_from_welcome` rejects a
    /// misrouted Welcome before any state advance.
    #[wasm_bindgen(js_name = joinFromWelcome)]
    pub fn join_from_welcome(
        welcome: &[u8],
        identity: &Identity,
        account_id: &[u8],
    ) -> Result<DeviceGroup, JsValue> {
        if account_id.len() != 16 {
            return Err(js_error("account_id must be 16 bytes"));
        }
        let mut acct = [0u8; 16];
        acct.copy_from_slice(account_id);
        let inner =
            MlsDeviceGroup::join_from_welcome(welcome, &identity.inner, &acct).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Add a device by its TLS-encoded KeyPackage. Returns
    /// `(commit, welcome)`: broadcast `commit` to existing
    /// members via the DS group journal, deliver `welcome` to
    /// the joining device via DS mailbox at the rendezvous
    /// pseudonym.
    #[wasm_bindgen(js_name = addDeviceFromWire)]
    pub fn add_device_from_wire(
        &self,
        identity: &Identity,
        key_package: &[u8],
    ) -> Result<AddMember, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let MemberAddition { commit, welcome } = g
            .add_device_from_wire(&identity.inner, key_package)
            .map_err(mls_err)?;
        Ok(AddMember {
            commit_bytes: group::encode_message(&commit).map_err(mls_err)?,
            welcome_bytes: group::encode_message(&welcome).map_err(mls_err)?,
        })
    }

    /// The ADR-0053 access signer for the DeviceGroup journal at the current
    /// epoch - the same derivation as `Group.dsAccessSigner` over the
    /// underlying group. Re-derive after every commit.
    #[wasm_bindgen(js_name = dsAccessSigner)]
    pub fn ds_access_signer(
        &self,
        identity: &Identity,
    ) -> Result<crate::ds_proof::DsProofSigner, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        crate::ds_proof::group_signer(g.as_group(), &identity.inner)
    }

    /// The deterministic `group_id` this DeviceGroup is bound
    /// to. Same value `deviceGroupIdFor(account_id)` returns.
    #[wasm_bindgen(js_name = groupId)]
    pub fn group_id(&self) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.group_id())
    }

    /// Snapshot of every device leaf currently in the
    /// DeviceGroup (Phase 2a3c). One [`GroupMember`] per row —
    /// the JS layer renders the list, marks the row whose
    /// `leafIndex` matches [`Self::ownLeafIndex`] as
    /// "(this device)", and offers a Remove affordance on every
    /// other row.
    ///
    /// Same wasm-bindgen shape as the conversation
    /// [`Group::members`] — flat `JsValue` array, ordering
    /// matches `lakko_mls::DeviceGroup::devices` (left-to-right
    /// over the leaf tree, stable inside a single epoch).
    #[wasm_bindgen]
    pub fn devices(&self) -> Result<Vec<JsValue>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.devices()
            .into_iter()
            .map(|m| {
                JsValue::from(GroupMember {
                    leaf_index_val: m.leaf_index,
                    signature_public_key_val: m.signature_public_key,
                    account: m.account_key,
                })
            })
            .collect())
    }

    /// Leaf index of THIS device inside the DeviceGroup
    /// (Phase 2a3c). UI uses this to flag the "(this device)"
    /// row and disable its Remove button.
    #[wasm_bindgen(js_name = ownLeafIndex)]
    pub fn own_leaf_index(&self) -> Result<u32, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.own_leaf_index())
    }

    /// Revoke a device leaf (Phase 2a3c). Returns the encoded
    /// Commit bytes the caller MUST broadcast to every remaining
    /// device via the DS group journal so they advance past the
    /// revoked leaf and ratchet the group secrets out of the
    /// removed device's reach.
    ///
    /// `leafIndex` must come from a [`Self::devices`] snapshot
    /// at the SAME epoch; concurrent commits can renumber
    /// leaves, in which case the caller refetches and retries.
    /// Attempting to remove the local device's own leaf is
    /// rejected with a wasm-side error.
    #[wasm_bindgen(js_name = removeDeviceAt)]
    pub fn remove_device_at(
        &self,
        identity: &Identity,
        leaf_index: u32,
    ) -> Result<Vec<u8>, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        g.remove_device_at(&identity.inner, leaf_index)
            .map_err(mls_err)
    }

    /// Derive the per-pairing history-bundle exporter secret
    /// (Phase 2a3d8). Returns 32 bytes — the scanner side feeds
    /// it into [`sealHistoryBundle`], the joining side feeds it
    /// into [`openHistoryBundle`]. Both devices reach the same
    /// value after they advance to the post-add-device epoch.
    #[wasm_bindgen(js_name = historyBundleSecret)]
    pub fn history_bundle_secret(&self, identity: &Identity) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let secret =
            lakko_mls::history_bundle::exporter_secret(&g, &identity.inner).map_err(mls_err)?;
        Ok(secret.to_vec())
    }

    /// Derive a `length`-byte secret bound to this DeviceGroup's current
    /// epoch (ADR-0018 Part 2a, factor (a) — "another-device" identity
    /// transfer, Path B). Mirror of `Group.exportSecret` / the UniFFI
    /// surface. A **read-only** MLS exporter call (no state mutation). After a
    /// fresh device joins via [`DeviceGroup::joinFromWelcome`], the existing
    /// and fresh device sit at the same epoch and derive the SAME bytes for
    /// the same `(label, context, length)`; the existing device seals
    /// `(grant_token ‖ phrase_seed)` under this key (via [`sealWithKey`]) and
    /// the fresh device opens it (via [`openWithKey`]).
    #[wasm_bindgen(js_name = exportSecret)]
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: &str,
        context: &[u8],
        length: u32,
    ) -> Result<Vec<u8>, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let secret = g
            .export_secret(&identity.inner, label, context, length as usize)
            .map_err(mls_err)?;
        Ok(secret.to_vec())
    }

    // ====================================================================
    // 2a3e2 — typed cross-device payload composer / consumer.
    //
    // wasm-bindgen rejects Rust enums with associated data on the
    // public surface, so each typed body variant gets its own
    // composer. The parser returns a flat [`ParsedDeviceGroupPayload`]
    // object whose getters surface a `kind` discriminator + every
    // possible field; the JS layer reads only the fields relevant to
    // the active kind. Mirror of the UniFFI surface (where the
    // FFI-friendly `DeviceGroupBody` Enum sealed-shape is idiomatic).
    // ====================================================================

    /// Compose a `ReadCursor` cross-device payload + return the
    /// encrypted wire bytes ready to PUT to the DeviceGroup DS
    /// journal (Phase 2a3e2). `groupId` / `upToMessageId` are
    /// raw bytes; `readAtMs` / `hlc` are unix-millis / HLC u64s.
    #[wasm_bindgen(js_name = createDeviceGroupReadCursorMessage)]
    pub fn create_devicegroup_read_cursor_message(
        &self,
        identity: &Identity,
        group_id: &[u8],
        up_to_message_id: &[u8],
        read_at_ms: u64,
        hlc: u64,
    ) -> Result<Vec<u8>, JsValue> {
        self.create_devicegroup_body(
            identity,
            lakko_mls::device_group::DeviceGroupBody::ReadCursor {
                group_id: group_id.to_vec(),
                up_to_message_id: up_to_message_id.to_vec(),
                read_at_ms,
            },
            hlc,
        )
    }

    /// Compose a `Draft` cross-device payload. Empty `text`
    /// clears the receiver's local draft for the same group.
    #[wasm_bindgen(js_name = createDeviceGroupDraftMessage)]
    pub fn create_devicegroup_draft_message(
        &self,
        identity: &Identity,
        group_id: &[u8],
        text: &str,
        reply_to_id: &[u8],
        hlc: u64,
    ) -> Result<Vec<u8>, JsValue> {
        self.create_devicegroup_body(
            identity,
            lakko_mls::device_group::DeviceGroupBody::Draft {
                group_id: group_id.to_vec(),
                text: text.to_string(),
                reply_to_id: reply_to_id.to_vec(),
            },
            hlc,
        )
    }

    /// Compose a `Flags` cross-device payload (pin / mute /
    /// archive / unread-override snapshot — last-write-wins by
    /// HLC).
    #[wasm_bindgen(js_name = createDeviceGroupFlagsMessage)]
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn create_devicegroup_flags_message(
        &self,
        identity: &Identity,
        group_id: &[u8],
        pinned: bool,
        muted: bool,
        archived: bool,
        unread_override: bool,
        hlc: u64,
    ) -> Result<Vec<u8>, JsValue> {
        self.create_devicegroup_body(
            identity,
            lakko_mls::device_group::DeviceGroupBody::Flags {
                group_id: group_id.to_vec(),
                pinned,
                muted,
                archived,
                unread_override,
            },
            hlc,
        )
    }

    /// Compose an `Order` cross-device payload — explicit
    /// display ordering for the conversation list. `groupIds`
    /// is the flat concatenation of all group_ids; the lengths
    /// are encoded inline via the leading-length prefix
    /// convention `len(u32 LE) || bytes`. Callers should
    /// prefer the `Vec<Vec<u8>>` shape; wasm-bindgen flattens
    /// it via a JS `Uint8Array[]`.
    #[wasm_bindgen(js_name = createDeviceGroupOrderMessage)]
    pub fn create_devicegroup_order_message(
        &self,
        identity: &Identity,
        group_ids: Vec<js_sys::Uint8Array>,
        hlc: u64,
    ) -> Result<Vec<u8>, JsValue> {
        let owned: Vec<Vec<u8>> = group_ids.into_iter().map(|a| a.to_vec()).collect();
        self.create_devicegroup_body(
            identity,
            lakko_mls::device_group::DeviceGroupBody::Order { group_ids: owned },
            hlc,
        )
    }

    /// Compose a `serviceEnvelope` cross-device payload carrying a
    /// `lakko-contacts` envelope verbatim (ADR-0060 section 7: the output
    /// of `composeVisibilityEnvelope` / `composeReachStateEnvelope`).
    ///
    /// # Errors
    ///
    /// Throws when `envelope` is empty or longer than
    /// `deviceGroupServiceEnvelopeMaxLen()`; nothing is encrypted.
    #[wasm_bindgen(js_name = createServiceEnvelopeMessage)]
    pub fn create_service_envelope_message(
        &self,
        identity: &Identity,
        envelope: &[u8],
        hlc: u64,
    ) -> Result<Vec<u8>, JsValue> {
        self.create_devicegroup_body(
            identity,
            lakko_mls::device_group::DeviceGroupBody::ServiceEnvelope {
                envelope: envelope.to_vec(),
            },
            hlc,
        )
    }

    /// Decrypt / advance the state machine for an inbound
    /// DeviceGroup wire message (Phase 2a3e2). JS layer reads
    /// `kind`:
    ///
    ///   * `"application"` — pass `data` into
    ///     [`parseDeviceGroupPayload`] to project into a typed
    ///     payload.
    ///   * `"commit"` — DeviceGroup advanced. Caller refreshes
    ///     `devices()`.
    ///   * `"proposal"` / `"externalJoinProposal"` — defensive;
    ///     no state advance.
    #[wasm_bindgen(js_name = processIncoming)]
    pub fn dg_process_incoming(
        &self,
        identity: &Identity,
        message_bytes: &[u8],
    ) -> Result<ProcessedMessage, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let incoming = g
            .process_incoming(&identity.inner, message_bytes)
            .map_err(mls_err)?;
        Ok(match incoming {
            lakko_mls::Incoming::Application {
                bytes,
                sender_leaf,
                sender_account,
            } => ProcessedMessage::application(bytes, sender_leaf, sender_account),
            lakko_mls::Incoming::Commit => ProcessedMessage::control(ProcessedMessageKind::Commit),
            lakko_mls::Incoming::Proposal(record) => ProcessedMessage::from_proposal(record),
            lakko_mls::Incoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::external_join(joiner_signature_key, retained),
        })
    }

    /// G-13 (ADR-0015): flip this DeviceGroup's policy to **require a
    /// peer-verified attestation for device removals**. Returns the encoded
    /// Commit to broadcast over the DS group journal. Call when the account
    /// first enables a passkey (OPAQUE-only accounts never call it).
    #[wasm_bindgen(js_name = setRequireAttestedRemovals)]
    pub fn set_require_attested_removals(&self, identity: &Identity) -> Result<Vec<u8>, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        g.set_require_attested_removals(&identity.inner)
            .map_err(mls_err)
    }

    /// G-13 (ADR-0015): whether this DeviceGroup currently requires attested
    /// removals (read from authenticated group state).
    #[wasm_bindgen(js_name = requireAttestedRemovals)]
    pub fn require_attested_removals(&self) -> Result<bool, JsValue> {
        let g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        Ok(g.require_attested_removals())
    }

    /// G-13 (ADR-0015): apply an incoming DeviceGroup commit **enforcing the
    /// attested-removal policy**. When the policy is on and the commit
    /// removes a leaf, `attestation` must be the attestation the auth service
    /// issued for that removal — verified under `attestVerifyingKey` (the
    /// 32-byte Ed25519 key from the device-attest JWKS), bound to this
    /// account + the removed leaf + fresh at `nowUnixSecs`. A removal without
    /// a valid attestation is refused (the commit is NOT merged). An empty
    /// `attestation` means "none supplied". Policy-off / non-remove commits
    /// behave like [`Self::dg_process_incoming`].
    #[wasm_bindgen(js_name = processIncomingVerifyingRemovals)]
    pub fn dg_process_incoming_verifying_removals(
        &self,
        identity: &Identity,
        message_bytes: &[u8],
        attestation: &[u8],
        attest_verifying_key: &[u8],
        now_unix_secs: i64,
    ) -> Result<ProcessedMessage, JsValue> {
        let vk = lakko_crypto::signing::VerifyingKey::from_bytes(attest_verifying_key)
            .map_err(|_| js_error("invalid attestation verifying key"))?;
        // The wasm surface carries a single attestation (the web remove-device
        // flow evicts one leaf); empty means none.
        let attestations: Vec<Vec<u8>> = if attestation.is_empty() {
            Vec::new()
        } else {
            vec![attestation.to_vec()]
        };
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        let incoming = g
            .process_incoming_verifying_removals(
                &identity.inner,
                message_bytes,
                &attestations,
                &vk,
                now_unix_secs,
            )
            .map_err(mls_err)?;
        Ok(match incoming {
            lakko_mls::Incoming::Application {
                bytes,
                sender_leaf,
                sender_account,
            } => ProcessedMessage::application(bytes, sender_leaf, sender_account),
            lakko_mls::Incoming::Commit => ProcessedMessage::control(ProcessedMessageKind::Commit),
            lakko_mls::Incoming::Proposal(record) => ProcessedMessage::from_proposal(record),
            lakko_mls::Incoming::ExternalJoinProposal {
                joiner_signature_key,
                retained,
            } => ProcessedMessage::external_join(joiner_signature_key, retained),
        })
    }

    /// Shared compose plumbing — wire-encodes via OpenMLS +
    /// returns the bytes.
    fn create_devicegroup_body(
        &self,
        identity: &Identity,
        body: lakko_mls::device_group::DeviceGroupBody,
        hlc: u64,
    ) -> Result<Vec<u8>, JsValue> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| js_error("internal lock poisoned"))?;
        g.create_devicegroup_message(&identity.inner, body, hlc)
            .map_err(mls_err)
    }
}

/// Discriminator string for [`ParsedDeviceGroupPayload::kind`].
/// JS-side `switch` matches on this to read the relevant
/// optional fields. Pinned ASCII; any change is a breaking
/// surface change.
const KIND_READ_CURSOR: &str = "readCursor";
const KIND_DRAFT: &str = "draft";
const KIND_FLAGS: &str = "flags";
const KIND_ORDER: &str = "order";
const KIND_SERVICE_ENVELOPE: &str = "serviceEnvelope";

/// Longest envelope a DeviceGroup `serviceEnvelope` payload carries
/// (ADR-0060 section 7).
#[wasm_bindgen(js_name = deviceGroupServiceEnvelopeMaxLen)]
#[must_use]
pub fn device_group_service_envelope_max_len() -> u32 {
    u32::try_from(lakko_mls::MAX_SERVICE_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

/// JS-side projection of
/// [`lakko_mls::device_group::ParsedDeviceGroupPayload`]
/// (Phase 2a3e2). One flat class with a `kind` discriminator +
/// every possible field as a typed getter — keeps the
/// wasm-bindgen surface simple at the cost of some
/// always-present fields on every variant.
///
/// Each field carries a trailing `_val` to disambiguate from
/// the same-named wasm-bindgen getter; this is mechanically
/// required by wasm-bindgen and outside our control.
#[wasm_bindgen]
#[allow(
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::struct_field_names
)]
pub struct ParsedDeviceGroupPayload {
    hlc_val: u64,
    sender_leaf_val: u32,
    kind_val: &'static str,
    group_id_val: Vec<u8>,
    up_to_message_id_val: Vec<u8>,
    read_at_ms_val: u64,
    text_val: String,
    reply_to_id_val: Vec<u8>,
    pinned_val: bool,
    muted_val: bool,
    archived_val: bool,
    unread_override_val: bool,
    order_group_ids_val: Vec<Vec<u8>>,
    service_envelope_val: Vec<u8>,
}

#[wasm_bindgen]
impl ParsedDeviceGroupPayload {
    /// HLC timestamp from the envelope.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.hlc_val
    }

    /// Leaf index of the device that authored this payload.
    #[wasm_bindgen(getter, js_name = senderLeaf)]
    pub fn sender_leaf(&self) -> u32 {
        self.sender_leaf_val
    }

    /// One of `"readCursor"` / `"draft"` / `"flags"` /
    /// `"order"` / `"serviceEnvelope"`. JS reads this to decide
    /// which other fields are meaningful.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind_val.to_string()
    }

    /// Conversation group_id. Set for `readCursor`, `draft`,
    /// and `flags`; empty for `order`.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.group_id_val.clone()
    }

    /// `readCursor` only: the message-id the sender claims to
    /// have read up to.
    #[wasm_bindgen(getter, js_name = upToMessageId)]
    pub fn up_to_message_id(&self) -> Vec<u8> {
        self.up_to_message_id_val.clone()
    }

    /// `readCursor` only: wall-clock UNIX millis when the read
    /// happened (display only).
    #[wasm_bindgen(getter, js_name = readAtMs)]
    pub fn read_at_ms(&self) -> u64 {
        self.read_at_ms_val
    }

    /// `draft` only: composer text. May be empty (clear).
    #[wasm_bindgen(getter)]
    pub fn text(&self) -> String {
        self.text_val.clone()
    }

    /// `draft` only: the message-id the draft is replying to,
    /// or empty.
    #[wasm_bindgen(getter, js_name = replyToId)]
    pub fn reply_to_id(&self) -> Vec<u8> {
        self.reply_to_id_val.clone()
    }

    /// `flags` only: pinned indicator.
    #[wasm_bindgen(getter)]
    pub fn pinned(&self) -> bool {
        self.pinned_val
    }

    /// `flags` only: muted indicator.
    #[wasm_bindgen(getter)]
    pub fn muted(&self) -> bool {
        self.muted_val
    }

    /// `flags` only: archived indicator.
    #[wasm_bindgen(getter)]
    pub fn archived(&self) -> bool {
        self.archived_val
    }

    /// `flags` only: manual unread-override indicator.
    #[wasm_bindgen(getter, js_name = unreadOverride)]
    pub fn unread_override(&self) -> bool {
        self.unread_override_val
    }

    /// `order` only: conversation group_ids in display order,
    /// top-to-bottom. Empty for non-`order` kinds.
    #[wasm_bindgen(js_name = orderGroupIds)]
    pub fn order_group_ids(&self) -> Vec<js_sys::Uint8Array> {
        self.order_group_ids_val
            .iter()
            .map(|g| js_sys::Uint8Array::from(g.as_slice()))
            .collect()
    }

    /// `serviceEnvelope` only: the `lakko-contacts` envelope bytes,
    /// uninterpreted (ADR-0060 section 7). Hand them to
    /// `dispatchIncomingEnvelope` and keep only Visibility / ReachState.
    /// Empty for other kinds.
    #[wasm_bindgen(getter, js_name = serviceEnvelope)]
    pub fn service_envelope(&self) -> Vec<u8> {
        self.service_envelope_val.clone()
    }
}

/// Parse a DeviceGroup application-payload plaintext (the
/// `data` field of `ProcessedMessage::Application`) into a
/// typed [`ParsedDeviceGroupPayload`] (Phase 2a3e2).
#[wasm_bindgen(js_name = parseDeviceGroupPayload)]
pub fn parse_devicegroup_payload(plaintext: &[u8]) -> Result<ParsedDeviceGroupPayload, JsValue> {
    use lakko_mls::device_group::DeviceGroupBody as Body;
    let parsed = lakko_mls::DeviceGroup::parse_devicegroup_payload(plaintext).map_err(mls_err)?;
    let mut out = ParsedDeviceGroupPayload {
        hlc_val: parsed.hlc,
        sender_leaf_val: parsed.sender_leaf,
        kind_val: KIND_READ_CURSOR,
        group_id_val: Vec::new(),
        up_to_message_id_val: Vec::new(),
        read_at_ms_val: 0,
        text_val: String::new(),
        reply_to_id_val: Vec::new(),
        pinned_val: false,
        muted_val: false,
        archived_val: false,
        unread_override_val: false,
        order_group_ids_val: Vec::new(),
        service_envelope_val: Vec::new(),
    };
    match parsed.body {
        Body::ReadCursor {
            group_id,
            up_to_message_id,
            read_at_ms,
        } => {
            out.kind_val = KIND_READ_CURSOR;
            out.group_id_val = group_id;
            out.up_to_message_id_val = up_to_message_id;
            out.read_at_ms_val = read_at_ms;
        },
        Body::Draft {
            group_id,
            text,
            reply_to_id,
        } => {
            out.kind_val = KIND_DRAFT;
            out.group_id_val = group_id;
            out.text_val = text;
            out.reply_to_id_val = reply_to_id;
        },
        Body::Flags {
            group_id,
            pinned,
            muted,
            archived,
            unread_override,
        } => {
            out.kind_val = KIND_FLAGS;
            out.group_id_val = group_id;
            out.pinned_val = pinned;
            out.muted_val = muted;
            out.archived_val = archived;
            out.unread_override_val = unread_override;
        },
        Body::Order { group_ids } => {
            out.kind_val = KIND_ORDER;
            out.order_group_ids_val = group_ids;
        },
        Body::ServiceEnvelope { envelope } => {
            out.kind_val = KIND_SERVICE_ENVELOPE;
            out.service_envelope_val = envelope;
        },
    }
    Ok(out)
}

/// AEAD-seal a [`lakko_proto::mls_app::history_bundle::v1::HistoryBundle`]
/// (Phase 2a3d8).
///
/// `secret` MUST be 32 bytes. `bundleBytes` is the prost-encoded
/// `HistoryBundle` proto bytes the caller built from its local
/// message store. Returns the wire-format `SealedBundle` proto
/// bytes ready to PUT to the rendezvous mailbox.
///
/// Refuses to seal bundles whose total message count exceeds
/// `lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES` (1000).
#[wasm_bindgen(js_name = sealHistoryBundle)]
pub fn seal_history_bundle(secret: &[u8], bundle_bytes: &[u8]) -> Result<Vec<u8>, JsValue> {
    use prost::Message as _;
    if secret.len() != 32 {
        return Err(js_error("history-bundle secret must be 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(secret);

    let bundle = lakko_proto::mls_app::history_bundle::v1::HistoryBundle::decode(bundle_bytes)
        .map_err(|e| js_error(&format!("HistoryBundle decode: {e}")))?;
    lakko_mls::history_bundle::seal_bundle(&key, &bundle).map_err(mls_err)
}

/// Inverse of [`sealHistoryBundle`]. Returns the prost-encoded
/// `HistoryBundle` bytes; the caller decodes via its own proto
/// runtime (`protobuf-ts` on TS).
#[wasm_bindgen(js_name = openHistoryBundle)]
pub fn open_history_bundle(secret: &[u8], sealed_bytes: &[u8]) -> Result<Vec<u8>, JsValue> {
    use prost::Message as _;
    if secret.len() != 32 {
        return Err(js_error("history-bundle secret must be 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(secret);

    let bundle = lakko_mls::history_bundle::open_bundle(&key, sealed_bytes).map_err(mls_err)?;
    let mut out = Vec::with_capacity(bundle.encoded_len());
    bundle
        .encode(&mut out)
        .map_err(|e| js_error(&format!("HistoryBundle re-encode: {e}")))?;
    Ok(out)
}

// ============================================================================
// Phase 2a3d8-ui — structured history-bundle API for the web
//
// wasm-bindgen rejects Rust enums-with-data + nested Vecs of Records in
// the public surface, so the structured API splits into:
//
//   - `HistoryMessageInput` / `HistoryGroupInput` — wasm-bindgen
//     classes the JS caller constructs and passes into the seal path.
//   - `OpenedHistoryGroup` / `OpenedHistoryMessage` — flat
//     wasm-bindgen classes the open path returns; the JS caller
//     iterates `.groups()` etc.
//
// Wire format is identical to the byte path — both surfaces produce /
// consume the same `SealedBundle` proto.
// ============================================================================

/// One message the scanner-side caller adds to a
/// [`HistoryGroupInput`] before sealing the bundle.
/// wasm-bindgen class — JS-side `new HistoryMessageInput(...)`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct HistoryMessageInput {
    payload_bytes_val: Vec<u8>,
    sender_leaf_val: u32,
    message_id_val: Vec<u8>,
    received_at_ms_val: u64,
}

#[wasm_bindgen]
impl HistoryMessageInput {
    /// JS constructor — one call per included message.
    #[wasm_bindgen(constructor)]
    pub fn new(
        payload_bytes: &[u8],
        sender_leaf: u32,
        message_id: &[u8],
        received_at_ms: u64,
    ) -> HistoryMessageInput {
        HistoryMessageInput {
            payload_bytes_val: payload_bytes.to_vec(),
            sender_leaf_val: sender_leaf,
            message_id_val: message_id.to_vec(),
            received_at_ms_val: received_at_ms,
        }
    }
}

/// One group's worth of history rows for the seal path.
/// wasm-bindgen class — JS-side `new HistoryGroupInput(...)`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct HistoryGroupInput {
    group_id_val: Vec<u8>,
    messages_val: Vec<HistoryMessageInput>,
    count_capped_val: bool,
}

#[wasm_bindgen]
impl HistoryGroupInput {
    /// JS constructor. `messages` is a flat `Array<HistoryMessageInput>`;
    /// the binding consumes ownership.
    #[wasm_bindgen(constructor)]
    pub fn new(
        group_id: &[u8],
        messages: Vec<HistoryMessageInput>,
        count_capped: bool,
    ) -> HistoryGroupInput {
        HistoryGroupInput {
            group_id_val: group_id.to_vec(),
            messages_val: messages,
            count_capped_val: count_capped,
        }
    }
}

/// AEAD-seal a structured history bundle (Phase 2a3d8-ui).
///
/// `secret` MUST be 32 bytes (typically the output of
/// [`DeviceGroup::historyBundleSecret`]). `groups` is an
/// `Array<HistoryGroupInput>`.
#[wasm_bindgen(js_name = sealHistoryBundleStructured)]
pub fn seal_history_bundle_structured(
    secret: &[u8],
    groups: Vec<HistoryGroupInput>,
    max_hlc: u64,
) -> Result<Vec<u8>, JsValue> {
    if secret.len() != 32 {
        return Err(js_error("history-bundle secret must be 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(secret);

    let bundle = build_history_bundle_proto(groups, max_hlc);
    lakko_mls::history_bundle::seal_bundle(&key, &bundle).map_err(mls_err)
}

fn build_history_bundle_proto(
    groups: Vec<HistoryGroupInput>,
    max_hlc: u64,
) -> lakko_proto::mls_app::history_bundle::v1::HistoryBundle {
    use lakko_proto::mls_app::history_bundle::v1::{HistoryBundle, HistoryGroup, HistoryMessage};
    HistoryBundle {
        max_hlc,
        groups: groups
            .into_iter()
            .map(|g| HistoryGroup {
                group_id: g.group_id_val,
                count_capped: g.count_capped_val,
                messages: g
                    .messages_val
                    .into_iter()
                    .map(|m| HistoryMessage {
                        payload_bytes: m.payload_bytes_val,
                        sender_leaf: m.sender_leaf_val,
                        message_id: m.message_id_val,
                        received_at_ms: m.received_at_ms_val,
                    })
                    .collect(),
            })
            .collect(),
    }
}

// ============================================================================
// History-bundle per-message placeholder codec (G-6)
//
// Canonical cross-platform encode/decode for the per-message
// `payload_bytes` display projection. The byte format is produced
// by the SAME `lakko_mls::history_placeholder` encoder on every
// platform (web here, native via UniFFI), so the bytes interop
// web <-> native — closing the divergence where each platform
// JSON-encoded its own `PersistedMessage` shape (known-gaps G-6).
//
// `direction` crosses the wasm boundary as a u8: 0 = outgoing,
// 1 = incoming, 2 = system (wasm-bindgen does not ergonomically
// round-trip a data-less enum through plain functions, and the
// web TS layer already maps these onto its string union).
// ============================================================================

fn direction_from_u8(tag: u8) -> Result<lakko_mls::history_placeholder::HistoryDirection, JsValue> {
    use lakko_mls::history_placeholder::HistoryDirection as D;
    match tag {
        0 => Ok(D::Outgoing),
        1 => Ok(D::Incoming),
        2 => Ok(D::System),
        other => Err(js_error(&format!(
            "history placeholder: direction must be 0/1/2, got {other}"
        ))),
    }
}

/// One attachment the caller adds to a placeholder before
/// encoding. `blobId` / `key` MUST be 32 bytes, `nonce` 12 bytes.
/// JS-side `new HistoryAttachmentInput(...)`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct HistoryAttachmentInput {
    blob_id_val: Vec<u8>,
    key_val: Vec<u8>,
    nonce_val: Vec<u8>,
    size_val: u64,
    mime_val: Option<String>,
    name_val: Option<String>,
    media_val: lakko_attachments::RawMediaMetadata,
    thumb_refs_val: Option<(Vec<u8>, Vec<u8>, Vec<u8>)>,
}

#[wasm_bindgen]
impl HistoryAttachmentInput {
    /// JS constructor — one call per attachment.
    #[wasm_bindgen(constructor)]
    pub fn new(
        blob_id: &[u8],
        key: &[u8],
        nonce: &[u8],
        size: u64,
        mime: Option<String>,
        name: Option<String>,
    ) -> HistoryAttachmentInput {
        HistoryAttachmentInput {
            blob_id_val: blob_id.to_vec(),
            key_val: key.to_vec(),
            nonce_val: nonce.to_vec(),
            size_val: size,
            mime_val: mime,
            name_val: name,
            media_val: lakko_attachments::RawMediaMetadata::default(),
            thumb_refs_val: None,
        }
    }

    /// ADR-0047 media metadata. `waveform` is the raw amplitude bytes.
    /// Out-of-range values are dropped on encode, as for a live envelope.
    #[wasm_bindgen(js_name = setMedia)]
    pub fn set_media(
        &mut self,
        kind: Option<String>,
        duration_ms: Option<u32>,
        waveform: Option<Vec<u8>>,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        self.media_val.kind = kind;
        self.media_val.duration_ms = duration_ms;
        self.media_val.waveform = waveform;
        self.media_val.width = width;
        self.media_val.height = height;
    }

    /// ADR-0047 poster frame. `blobId` / `key` MUST be 32 bytes, `nonce`
    /// 12 bytes.
    #[wasm_bindgen(js_name = setThumb)]
    #[allow(clippy::too_many_arguments)]
    pub fn set_thumb(
        &mut self,
        blob_id: &[u8],
        key: &[u8],
        nonce: &[u8],
        size: u64,
        mime: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        self.thumb_refs_val = Some((blob_id.to_vec(), key.to_vec(), nonce.to_vec()));
        self.media_val.thumb = Some(lakko_attachments::RawThumbMetadata {
            size,
            mime,
            width,
            height,
        });
    }
}

/// Apply the ADR-0047 bounds (`lakko_attachments::RawMediaMetadata`) to a
/// history attachment, so a restored message keeps exactly what the live
/// envelope would have kept.
fn bound_history_media(
    mut a: lakko_mls::history_placeholder::HistoryAttachmentPlaceholder,
) -> lakko_mls::history_placeholder::HistoryAttachmentPlaceholder {
    let raw = lakko_attachments::RawMediaMetadata {
        kind: a.kind.take(),
        duration_ms: a.duration_ms,
        waveform: a.waveform.take(),
        width: a.width,
        height: a.height,
        thumb: a
            .thumb
            .as_ref()
            .map(|t| lakko_attachments::RawThumbMetadata {
                size: t.size,
                mime: t.mime.clone(),
                width: t.width,
                height: t.height,
            }),
    }
    .bounded();
    a.kind = raw.kind;
    a.duration_ms = raw.duration_ms;
    a.waveform = raw.waveform;
    a.width = raw.width;
    a.height = raw.height;
    a.thumb = match (a.thumb.take(), raw.thumb) {
        (Some(mut t), Some(bounded)) => {
            t.mime = bounded.mime;
            t.width = bounded.width;
            t.height = bounded.height;
            Some(t)
        },
        _ => None,
    };
    a
}

fn fixed_bytes<const N: usize>(v: &[u8], field: &str) -> Result<[u8; N], JsValue> {
    if v.len() != N {
        return Err(js_error(&format!(
            "history placeholder: {field} must be {N} bytes, got {}",
            v.len()
        )));
    }
    let mut arr = [0u8; N];
    arr.copy_from_slice(v);
    Ok(arr)
}

/// Encode a placeholder to the canonical `payload_bytes` wire
/// bytes. `direction` is 0 = outgoing, 1 = incoming, 2 = system.
/// `attachments` is an `Array<HistoryAttachmentInput>`.
#[wasm_bindgen(js_name = encodeHistoryPlaceholder)]
pub fn encode_history_placeholder(
    message_id: u64,
    direction: u8,
    created_at: &str,
    text: &str,
    attachments: Vec<HistoryAttachmentInput>,
) -> Result<Vec<u8>, JsValue> {
    use lakko_mls::history_placeholder::{
        HistoryAttachmentPlaceholder, HistoryMessagePlaceholder, HistoryThumbPlaceholder,
    };
    let dir = direction_from_u8(direction)?;
    let mut atts = Vec::with_capacity(attachments.len());
    for a in attachments {
        let media = a.media_val;
        let thumb = match (a.thumb_refs_val, media.thumb) {
            (Some((blob_id, key, nonce)), Some(t)) => Some(HistoryThumbPlaceholder {
                blob_id: fixed_bytes::<32>(&blob_id, "thumb blob_id")?,
                key: fixed_bytes::<32>(&key, "thumb key")?,
                nonce: fixed_bytes::<12>(&nonce, "thumb nonce")?,
                size: t.size,
                mime: t.mime,
                width: t.width,
                height: t.height,
            }),
            _ => None,
        };
        atts.push(bound_history_media(HistoryAttachmentPlaceholder {
            blob_id: fixed_bytes::<32>(&a.blob_id_val, "blob_id")?,
            key: fixed_bytes::<32>(&a.key_val, "key")?,
            nonce: fixed_bytes::<12>(&a.nonce_val, "nonce")?,
            size: a.size_val,
            mime: a.mime_val,
            name: a.name_val,
            kind: media.kind,
            duration_ms: media.duration_ms,
            waveform: media.waveform,
            width: media.width,
            height: media.height,
            thumb,
        }));
    }
    let msg = HistoryMessagePlaceholder {
        message_id,
        direction: dir,
        created_at: created_at.to_string(),
        text: text.to_string(),
        attachments: atts,
    };
    msg.encode().map_err(mls_err)
}

/// One attachment after the decode path — flat getters.
#[wasm_bindgen]
#[allow(clippy::struct_field_names, missing_docs)]
#[derive(Clone)]
pub struct OpenedHistoryAttachment {
    blob_id_val: Vec<u8>,
    key_val: Vec<u8>,
    nonce_val: Vec<u8>,
    size_val: u64,
    mime_val: Option<String>,
    name_val: Option<String>,
    kind_val: Option<String>,
    duration_ms_val: Option<u32>,
    waveform_val: Option<Vec<u8>>,
    width_val: Option<u32>,
    height_val: Option<u32>,
    thumb_val: Option<OpenedHistoryThumb>,
}

/// A decoded poster frame (ADR-0047) — flat getters.
#[wasm_bindgen]
#[derive(Clone)]
#[allow(clippy::struct_field_names, missing_docs)]
pub struct OpenedHistoryThumb {
    blob_id_val: Vec<u8>,
    key_val: Vec<u8>,
    nonce_val: Vec<u8>,
    size_val: u64,
    mime_val: Option<String>,
    width_val: Option<u32>,
    height_val: Option<u32>,
}

#[wasm_bindgen]
#[allow(missing_docs)]
impl OpenedHistoryThumb {
    #[wasm_bindgen(getter, js_name = blobId)]
    pub fn blob_id(&self) -> Vec<u8> {
        self.blob_id_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn key(&self) -> Vec<u8> {
        self.key_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn nonce(&self) -> Vec<u8> {
        self.nonce_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn size(&self) -> u64 {
        self.size_val
    }
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> Option<String> {
        self.mime_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> Option<u32> {
        self.width_val
    }
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> Option<u32> {
        self.height_val
    }
}

#[wasm_bindgen]
#[allow(missing_docs)]
impl OpenedHistoryAttachment {
    #[wasm_bindgen(getter, js_name = blobId)]
    pub fn blob_id(&self) -> Vec<u8> {
        self.blob_id_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn key(&self) -> Vec<u8> {
        self.key_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn nonce(&self) -> Vec<u8> {
        self.nonce_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn size(&self) -> u64 {
        self.size_val
    }
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> Option<String> {
        self.mime_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> Option<String> {
        self.name_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> Option<String> {
        self.kind_val.clone()
    }
    #[wasm_bindgen(getter, js_name = durationMs)]
    pub fn duration_ms(&self) -> Option<u32> {
        self.duration_ms_val
    }
    /// Raw waveform amplitude bytes.
    #[wasm_bindgen(getter)]
    pub fn waveform(&self) -> Option<Vec<u8>> {
        self.waveform_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> Option<u32> {
        self.width_val
    }
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> Option<u32> {
        self.height_val
    }
    #[wasm_bindgen(getter)]
    pub fn thumb(&self) -> Option<OpenedHistoryThumb> {
        self.thumb_val.clone()
    }
}

/// A decoded placeholder — flat getters for the JS caller.
/// `direction` is 0 = outgoing, 1 = incoming, 2 = system.
#[wasm_bindgen]
#[allow(clippy::struct_field_names, missing_docs)]
pub struct OpenedHistoryPlaceholder {
    message_id_val: u64,
    direction_val: u8,
    created_at_val: String,
    text_val: String,
    attachments_val: Vec<OpenedHistoryAttachment>,
}

#[wasm_bindgen]
#[allow(missing_docs)]
impl OpenedHistoryPlaceholder {
    #[wasm_bindgen(getter, js_name = messageId)]
    pub fn message_id(&self) -> u64 {
        self.message_id_val
    }
    #[wasm_bindgen(getter)]
    pub fn direction(&self) -> u8 {
        self.direction_val
    }
    #[wasm_bindgen(getter, js_name = createdAt)]
    pub fn created_at(&self) -> String {
        self.created_at_val.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn text(&self) -> String {
        self.text_val.clone()
    }
    /// `Array<OpenedHistoryAttachment>`.
    #[wasm_bindgen(getter)]
    pub fn attachments(&self) -> Vec<OpenedHistoryAttachment> {
        self.attachments_val.clone()
    }
}

/// Decode canonical `payload_bytes` (produced by
/// `encodeHistoryPlaceholder` on any platform) into an
/// [`OpenedHistoryPlaceholder`].
#[wasm_bindgen(js_name = decodeHistoryPlaceholder)]
pub fn decode_history_placeholder(bytes: &[u8]) -> Result<OpenedHistoryPlaceholder, JsValue> {
    let msg = lakko_mls::history_placeholder::HistoryMessagePlaceholder::decode(bytes)
        .map_err(mls_err)?;
    Ok(OpenedHistoryPlaceholder {
        message_id_val: msg.message_id,
        direction_val: msg.direction.tag(),
        created_at_val: msg.created_at,
        text_val: msg.text,
        attachments_val: msg
            .attachments
            .into_iter()
            .map(bound_history_media)
            .map(|a| OpenedHistoryAttachment {
                blob_id_val: a.blob_id.to_vec(),
                key_val: a.key.to_vec(),
                nonce_val: a.nonce.to_vec(),
                size_val: a.size,
                mime_val: a.mime,
                name_val: a.name,
                kind_val: a.kind,
                duration_ms_val: a.duration_ms,
                waveform_val: a.waveform,
                width_val: a.width,
                height_val: a.height,
                thumb_val: a.thumb.map(|t| OpenedHistoryThumb {
                    blob_id_val: t.blob_id.to_vec(),
                    key_val: t.key.to_vec(),
                    nonce_val: t.nonce.to_vec(),
                    size_val: t.size,
                    mime_val: t.mime,
                    width_val: t.width,
                    height_val: t.height,
                }),
            })
            .collect(),
    })
}

/// One message after the open path — flat getters for the JS
/// caller. wasm-bindgen doesn't expose tuple-style returns, so
/// each opened message is a class instance.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct OpenedHistoryMessage {
    payload_bytes_val: Vec<u8>,
    sender_leaf_val: u32,
    message_id_val: Vec<u8>,
    received_at_ms_val: u64,
}

#[allow(missing_docs)]
#[wasm_bindgen]
impl OpenedHistoryMessage {
    #[wasm_bindgen(getter, js_name = payloadBytes)]
    pub fn payload_bytes(&self) -> Vec<u8> {
        self.payload_bytes_val.clone()
    }

    #[wasm_bindgen(getter, js_name = senderLeaf)]
    pub fn sender_leaf(&self) -> u32 {
        self.sender_leaf_val
    }

    #[wasm_bindgen(getter, js_name = messageId)]
    pub fn message_id(&self) -> Vec<u8> {
        self.message_id_val.clone()
    }

    #[wasm_bindgen(getter, js_name = receivedAtMs)]
    pub fn received_at_ms(&self) -> u64 {
        self.received_at_ms_val
    }
}

/// One group's worth of opened history rows for the receive
/// path. wasm-bindgen class — JS reads `.groupId`, `.messages()`,
/// `.countCapped`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct OpenedHistoryGroup {
    group_id_val: Vec<u8>,
    messages_val: Vec<OpenedHistoryMessage>,
    count_capped_val: bool,
}

#[wasm_bindgen]
impl OpenedHistoryGroup {
    /// MLS group_id this group's history rows belong to.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.group_id_val.clone()
    }

    /// Flat array of [`OpenedHistoryMessage`] — JS iterates via
    /// `Array.from(g.messages())`. wasm-bindgen owns the
    /// inner allocations; the caller must not retain references
    /// past the parent `OpenedHistoryGroup`'s lifetime.
    #[wasm_bindgen]
    pub fn messages(&self) -> Result<Vec<JsValue>, JsValue> {
        Ok(self
            .messages_val
            .iter()
            .map(|m| {
                JsValue::from(OpenedHistoryMessage {
                    payload_bytes_val: m.payload_bytes_val.clone(),
                    sender_leaf_val: m.sender_leaf_val,
                    message_id_val: m.message_id_val.clone(),
                    received_at_ms_val: m.received_at_ms_val,
                })
            })
            .collect())
    }

    /// `true` when the per-group message list was truncated
    /// to the producer-side cap. UI surfaces a "load older"
    /// affordance later.
    #[wasm_bindgen(getter, js_name = countCapped)]
    pub fn count_capped(&self) -> bool {
        self.count_capped_val
    }
}

/// Top-level result of [`openHistoryBundleStructured`].
/// wasm-bindgen class — JS reads `.groups()` and `.maxHlc`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct OpenedHistoryBundle {
    groups_val: Vec<OpenedHistoryGroup>,
    max_hlc_val: u64,
}

#[wasm_bindgen]
impl OpenedHistoryBundle {
    /// Flat array of [`OpenedHistoryGroup`]. Same JS-iter
    /// pattern as `OpenedHistoryGroup.messages()`.
    #[wasm_bindgen]
    pub fn groups(&self) -> Result<Vec<JsValue>, JsValue> {
        Ok(self
            .groups_val
            .iter()
            .map(|g| {
                JsValue::from(OpenedHistoryGroup {
                    group_id_val: g.group_id_val.clone(),
                    messages_val: g
                        .messages_val
                        .iter()
                        .map(|m| OpenedHistoryMessage {
                            payload_bytes_val: m.payload_bytes_val.clone(),
                            sender_leaf_val: m.sender_leaf_val,
                            message_id_val: m.message_id_val.clone(),
                            received_at_ms_val: m.received_at_ms_val,
                        })
                        .collect(),
                    count_capped_val: g.count_capped_val,
                })
            })
            .collect())
    }

    /// HLC of the most recent message included in the bundle.
    #[wasm_bindgen(getter, js_name = maxHlc)]
    pub fn max_hlc(&self) -> u64 {
        self.max_hlc_val
    }
}

/// Inverse of [`sealHistoryBundleStructured`]. Returns a flat
/// JS object the caller iterates through getters; no proto
/// runtime needed.
#[wasm_bindgen(js_name = openHistoryBundleStructured)]
pub fn open_history_bundle_structured(
    secret: &[u8],
    sealed_bytes: &[u8],
) -> Result<OpenedHistoryBundle, JsValue> {
    if secret.len() != 32 {
        return Err(js_error("history-bundle secret must be 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(secret);

    let bundle = lakko_mls::history_bundle::open_bundle(&key, sealed_bytes).map_err(mls_err)?;
    Ok(OpenedHistoryBundle {
        max_hlc_val: bundle.max_hlc,
        groups_val: bundle
            .groups
            .into_iter()
            .map(|g| OpenedHistoryGroup {
                group_id_val: g.group_id,
                count_capped_val: g.count_capped,
                messages_val: g
                    .messages
                    .into_iter()
                    .map(|m| OpenedHistoryMessage {
                        payload_bytes_val: m.payload_bytes,
                        sender_leaf_val: m.sender_leaf,
                        message_id_val: m.message_id,
                        received_at_ms_val: m.received_at_ms,
                    })
                    .collect(),
            })
            .collect(),
    })
}

/// G-13 (ADR-0015): the action hash a remove-device passkey ceremony and its
/// attestation bind to — `SHA-256("lakko/action/remove-device/v1" ‖
/// removed_leaf_signature_pubkey)`. The web client computes this from the
/// leaf it is about to remove to drive the `/auth/action/{start,finish}`
/// ceremony, so it matches what a peer recomputes when verifying.
#[wasm_bindgen(js_name = removeDeviceActionHash)]
pub fn remove_device_action_hash(removed_signature_pubkey: &[u8]) -> Vec<u8> {
    lakko_mls::device_group::remove_device_action_hash(removed_signature_pubkey).to_vec()
}

/// Compute the deterministic DeviceGroup `group_id` from an
/// account_id, without constructing a `DeviceGroup` handle.
/// Useful on the joining-device side when polling the DS
/// journal for catch-up Commits before any Welcome has been
/// processed.
#[wasm_bindgen(js_name = deviceGroupIdFor)]
pub fn device_group_id_for(account_id: &[u8]) -> Result<Vec<u8>, JsValue> {
    if account_id.len() != 16 {
        return Err(js_error("account_id must be 16 bytes"));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(account_id);
    Ok(MlsDeviceGroup::group_id_for(&acct))
}

/// Load an existing DeviceGroup for `account_id` from
/// `identity`'s MLS provider storage. Pair with
/// `Identity.restoreStorage` on app startup: restore the
/// snapshot first, then call this to get the live handle.
///
/// Returns `undefined` when the provider has no row under the
/// deterministic group_id — typical case: a freshly-derived
/// Identity that hasn't joined the DeviceGroup yet.
///
/// Free function instead of a `DeviceGroup::load` constructor
/// because wasm-bindgen's constructor surface doesn't tolerate
/// `Result<Option<Self>, _>` returns cleanly — the same shape
/// `Group::load` uses works fine as a free function.
#[wasm_bindgen(js_name = loadDeviceGroup)]
pub fn load_device_group(
    identity: &Identity,
    account_id: &[u8],
) -> Result<Option<DeviceGroup>, JsValue> {
    if account_id.len() != 16 {
        return Err(js_error("account_id must be 16 bytes"));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(account_id);
    match MlsDeviceGroup::load(&identity.inner, &acct).map_err(mls_err)? {
        Some(inner) => Ok(Some(DeviceGroup {
            inner: Mutex::new(inner),
        })),
        None => Ok(None),
    }
}

// ============================================================================
// Tests
// ============================================================================
//
// ============================================================================
// Phase 3a4 — Call signaling + SFrame keying FFI for the web
// ============================================================================

/// One composed call-signaling event — flat class with a
/// `kind` discriminator + every possible field, mirroring
/// the `ParsedDeviceGroupPayload` shape from 2a3e2a.
#[wasm_bindgen]
#[allow(
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::struct_field_names
)]
pub struct ParsedCallSignaling {
    kind_val: &'static str,
    call_id_val: Vec<u8>,
    sdp_val: String,
    created_at_ms_val: u64,
    media_audio_val: bool,
    media_video_val: bool,
    candidate_val: String,
    sdp_mid_val: String,
    sdp_m_line_index_val: u32,
    end_reason_val: &'static str,
    // Phase 3c2 — group-call variants.
    leaf_index_val: u32,
    initiator_leaf_val: u32,
    joined_at_ms_val: u64,
}

const CALL_KIND_OFFER: &str = "offer";
const CALL_KIND_ANSWER: &str = "answer";
const CALL_KIND_ICE_CANDIDATE: &str = "iceCandidate";
const CALL_KIND_END: &str = "end";
const CALL_KIND_GROUP_START: &str = "groupStart";
const CALL_KIND_GROUP_JOIN: &str = "groupJoin";
const CALL_KIND_GROUP_LEAVE: &str = "groupLeave";

#[wasm_bindgen]
impl ParsedCallSignaling {
    /// One of `"offer"` / `"answer"` / `"iceCandidate"` /
    /// `"end"`. JS reads this to decide which other fields
    /// are meaningful.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind_val.to_string()
    }

    /// 16-byte call_id (every kind carries one).
    #[wasm_bindgen(getter, js_name = callId)]
    pub fn call_id(&self) -> Vec<u8> {
        self.call_id_val.clone()
    }

    /// `offer` / `answer` only: SDP body.
    #[wasm_bindgen(getter)]
    pub fn sdp(&self) -> String {
        self.sdp_val.clone()
    }

    /// `offer` only: wall-clock UNIX millis.
    #[wasm_bindgen(getter, js_name = createdAtMs)]
    pub fn created_at_ms(&self) -> u64 {
        self.created_at_ms_val
    }

    /// `offer` only: audio media flag.
    #[wasm_bindgen(getter, js_name = mediaAudio)]
    pub fn media_audio(&self) -> bool {
        self.media_audio_val
    }

    /// `offer` only: video media flag.
    #[wasm_bindgen(getter, js_name = mediaVideo)]
    pub fn media_video(&self) -> bool {
        self.media_video_val
    }

    /// `iceCandidate` only: ICE candidate SDP fragment.
    #[wasm_bindgen(getter)]
    pub fn candidate(&self) -> String {
        self.candidate_val.clone()
    }

    /// `iceCandidate` only: SDP m-section identifier.
    #[wasm_bindgen(getter, js_name = sdpMid)]
    pub fn sdp_mid(&self) -> String {
        self.sdp_mid_val.clone()
    }

    /// `iceCandidate` only: zero-based SDP m-section index.
    #[wasm_bindgen(getter, js_name = sdpMLineIndex)]
    pub fn sdp_m_line_index(&self) -> u32 {
        self.sdp_m_line_index_val
    }

    /// `end` / `groupLeave` only: one of `"hungUp"` /
    /// `"busy"` / `"declined"` / `"timedOut"` / `"error"`.
    /// For `groupLeave` only `"hungUp"` / `"timedOut"` /
    /// `"error"` are meaningful.
    #[wasm_bindgen(getter, js_name = endReason)]
    pub fn end_reason(&self) -> String {
        self.end_reason_val.to_string()
    }

    /// `groupStart` only: MLS leaf index of the initiator.
    #[wasm_bindgen(getter, js_name = initiatorLeaf)]
    pub fn initiator_leaf(&self) -> u32 {
        self.initiator_leaf_val
    }

    /// `groupJoin` / `groupLeave` only: MLS leaf index of
    /// the joining / leaving participant.
    #[wasm_bindgen(getter, js_name = leafIndex)]
    pub fn leaf_index(&self) -> u32 {
        self.leaf_index_val
    }

    /// `groupJoin` only: wall-clock UNIX millis when the
    /// join landed on the chat journal.
    #[wasm_bindgen(getter, js_name = joinedAtMs)]
    pub fn joined_at_ms(&self) -> u64 {
        self.joined_at_ms_val
    }
}

/// Compose an Offer call-signaling event into plaintext
/// bytes (Phase 3a3).
#[wasm_bindgen(js_name = composeCallOfferSignaling)]
pub fn compose_call_offer_signaling(
    call_id: &[u8],
    sdp: &str,
    created_at_ms: u64,
    media_audio: bool,
    media_video: bool,
) -> Result<Vec<u8>, JsValue> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::Offer {
        call_id: id,
        sdp: sdp.to_string(),
        created_at_ms,
        media: lakko_calls::MediaFlags {
            audio: media_audio,
            video: media_video,
        },
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose an Answer call-signaling event.
#[wasm_bindgen(js_name = composeCallAnswerSignaling)]
pub fn compose_call_answer_signaling(call_id: &[u8], sdp: &str) -> Result<Vec<u8>, JsValue> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::Answer {
        call_id: id,
        sdp: sdp.to_string(),
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose an IceCandidate call-signaling event.
#[wasm_bindgen(js_name = composeCallIceCandidateSignaling)]
pub fn compose_call_ice_candidate_signaling(
    call_id: &[u8],
    candidate: &str,
    sdp_mid: &str,
    sdp_m_line_index: u32,
) -> Result<Vec<u8>, JsValue> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::IceCandidate {
        call_id: id,
        candidate: candidate.to_string(),
        sdp_mid: sdp_mid.to_string(),
        sdp_m_line_index,
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose an End call-signaling event. `reason` is one of
/// `"hungUp"` / `"busy"` / `"declined"` / `"timedOut"` /
/// `"error"`; unknown values fold to `"hungUp"`.
#[wasm_bindgen(js_name = composeCallEndSignaling)]
pub fn compose_call_end_signaling(call_id: &[u8], reason: &str) -> Result<Vec<u8>, JsValue> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::End {
        call_id: id,
        reason: parse_end_reason(reason),
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose a `GroupCallStart` call-signaling event
/// (Phase 3c2). Posted by the initiator to the chat MLS
/// group; observers render a joinable-call card.
#[wasm_bindgen(js_name = composeGroupCallStartSignaling)]
pub fn compose_group_call_start_signaling(
    call_id: &[u8],
    initiator_leaf: u32,
    created_at_ms: u64,
    media_audio: bool,
    media_video: bool,
) -> Result<Vec<u8>, JsValue> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::GroupStart {
        call_id: id,
        initiator_leaf,
        created_at_ms,
        media: lakko_calls::MediaFlags {
            audio: media_audio,
            video: media_video,
        },
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose a `GroupCallJoin` event (Phase 3c2). Sent by
/// each participant after their per-participant SFU
/// session reaches `connected`.
#[wasm_bindgen(js_name = composeGroupCallJoinSignaling)]
pub fn compose_group_call_join_signaling(
    call_id: &[u8],
    leaf_index: u32,
    joined_at_ms: u64,
) -> Result<Vec<u8>, JsValue> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::GroupJoin {
        call_id: id,
        leaf_index,
        joined_at_ms,
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose a `GroupCallLeave` event (Phase 3c2). `reason`
/// accepts the same values as `composeCallEndSignaling`;
/// only `"hungUp"` / `"timedOut"` / `"error"` are
/// meaningful for group-call leave.
#[wasm_bindgen(js_name = composeGroupCallLeaveSignaling)]
pub fn compose_group_call_leave_signaling(
    call_id: &[u8],
    leaf_index: u32,
    reason: &str,
) -> Result<Vec<u8>, JsValue> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::GroupLeave {
        call_id: id,
        leaf_index,
        reason: parse_end_reason(reason),
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Parse plaintext bytes (the `Application` payload from
/// `processIncoming`) into a typed
/// [`ParsedCallSignaling`] (Phase 3a3).
#[wasm_bindgen(js_name = parseCallSignaling)]
pub fn parse_call_signaling(plaintext: &[u8]) -> Result<ParsedCallSignaling, JsValue> {
    let body = lakko_calls::parse_signaling_payload(plaintext).map_err(calls_err)?;
    let mut out = ParsedCallSignaling {
        kind_val: CALL_KIND_OFFER,
        call_id_val: Vec::new(),
        sdp_val: String::new(),
        created_at_ms_val: 0,
        media_audio_val: false,
        media_video_val: false,
        candidate_val: String::new(),
        sdp_mid_val: String::new(),
        sdp_m_line_index_val: 0,
        end_reason_val: "hungUp",
        leaf_index_val: 0,
        initiator_leaf_val: 0,
        joined_at_ms_val: 0,
    };
    match body {
        lakko_calls::SignalingBody::Offer {
            call_id,
            sdp,
            created_at_ms,
            media,
        } => {
            out.kind_val = CALL_KIND_OFFER;
            out.call_id_val = call_id.to_vec();
            out.sdp_val = sdp;
            out.created_at_ms_val = created_at_ms;
            out.media_audio_val = media.audio;
            out.media_video_val = media.video;
        },
        lakko_calls::SignalingBody::Answer { call_id, sdp } => {
            out.kind_val = CALL_KIND_ANSWER;
            out.call_id_val = call_id.to_vec();
            out.sdp_val = sdp;
        },
        lakko_calls::SignalingBody::IceCandidate {
            call_id,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => {
            out.kind_val = CALL_KIND_ICE_CANDIDATE;
            out.call_id_val = call_id.to_vec();
            out.candidate_val = candidate;
            out.sdp_mid_val = sdp_mid;
            out.sdp_m_line_index_val = sdp_m_line_index;
        },
        lakko_calls::SignalingBody::End { call_id, reason } => {
            out.kind_val = CALL_KIND_END;
            out.call_id_val = call_id.to_vec();
            out.end_reason_val = end_reason_str(reason);
        },
        lakko_calls::SignalingBody::GroupStart {
            call_id,
            initiator_leaf,
            created_at_ms,
            media,
        } => {
            out.kind_val = CALL_KIND_GROUP_START;
            out.call_id_val = call_id.to_vec();
            out.initiator_leaf_val = initiator_leaf;
            out.created_at_ms_val = created_at_ms;
            out.media_audio_val = media.audio;
            out.media_video_val = media.video;
        },
        lakko_calls::SignalingBody::GroupJoin {
            call_id,
            leaf_index,
            joined_at_ms,
        } => {
            out.kind_val = CALL_KIND_GROUP_JOIN;
            out.call_id_val = call_id.to_vec();
            out.leaf_index_val = leaf_index;
            out.joined_at_ms_val = joined_at_ms;
        },
        lakko_calls::SignalingBody::GroupLeave {
            call_id,
            leaf_index,
            reason,
        } => {
            out.kind_val = CALL_KIND_GROUP_LEAVE;
            out.call_id_val = call_id.to_vec();
            out.leaf_index_val = leaf_index;
            out.end_reason_val = end_reason_str(reason);
        },
    }
    Ok(out)
}

fn take_call_id(bytes: &[u8]) -> Result<lakko_calls::CallId, JsValue> {
    if bytes.len() != 16 {
        return Err(js_error(&format!(
            "call_id length: expected 16, got {}",
            bytes.len()
        )));
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(bytes);
    Ok(id)
}

fn parse_end_reason(s: &str) -> lakko_calls::EndReason {
    match s {
        "busy" => lakko_calls::EndReason::Busy,
        "declined" => lakko_calls::EndReason::Declined,
        "timedOut" => lakko_calls::EndReason::TimedOut,
        "error" => lakko_calls::EndReason::Error,
        _ => lakko_calls::EndReason::HungUp,
    }
}

fn end_reason_str(r: lakko_calls::EndReason) -> &'static str {
    match r {
        lakko_calls::EndReason::HungUp => "hungUp",
        lakko_calls::EndReason::Busy => "busy",
        lakko_calls::EndReason::Declined => "declined",
        lakko_calls::EndReason::TimedOut => "timedOut",
        lakko_calls::EndReason::Error => "error",
    }
}

fn calls_err(err: lakko_calls::Error) -> JsValue {
    js_error(&format!("{err}"))
}

/// Per-participant SFrame key + nonce salt (Phase 3a4).
/// wasm-bindgen class with two getters.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct SframeParticipantKey {
    key_bytes_val: Vec<u8>,
    nonce_salt_val: Vec<u8>,
}

#[wasm_bindgen]
impl SframeParticipantKey {
    /// 32-byte AEAD key.
    #[wasm_bindgen(getter, js_name = keyBytes)]
    pub fn key_bytes(&self) -> Vec<u8> {
        self.key_bytes_val.clone()
    }

    /// 12-byte nonce salt — XOR with `(counter as u32 BE in
    /// the low 4 bytes)` to compose each per-frame nonce.
    #[wasm_bindgen(getter, js_name = nonceSalt)]
    pub fn nonce_salt(&self) -> Vec<u8> {
        self.nonce_salt_val.clone()
    }
}

/// Derive the per-call SFrame seed from a conversation MLS
/// group's exporter (Phase 3a4).
#[wasm_bindgen(js_name = sframeDeriveCallKey)]
pub fn sframe_derive_call_key(
    group: &Group,
    identity: &Identity,
    call_id: &[u8],
) -> Result<Vec<u8>, JsValue> {
    let g = group
        .inner
        .lock()
        .map_err(|_| js_error("internal lock poisoned"))?;
    let key = lakko_calls::CallKey::derive(&g, &identity.inner, call_id).map_err(calls_err)?;
    Ok(key.as_bytes().to_vec())
}

/// Derive a per-participant SFrame key + nonce salt for a given
/// `(sender_leaf, stream_index)` stream (ADR-0035). The seal-side derivation:
/// each of a leaf's concurrent streams (audio, camera, screen-share, …) gets a
/// distinct key + nonce salt keyed on the composite SFrame KID
/// `(sender_leaf << 8) | stream_index`, so their independent counters cannot
/// reuse a `(key, nonce)` pair — the guarantee the ADR-0034 per-media-TYPE tag
/// could not give two same-kind streams. `stream_index` crosses as a `u32`; a
/// value `> 255` fails closed rather than silently truncating.
#[wasm_bindgen(js_name = sframeDeriveParticipantKey)]
pub fn sframe_derive_participant_key(
    call_key_seed: &[u8],
    sender_leaf: u32,
    stream_index: u32,
) -> Result<SframeParticipantKey, JsValue> {
    if call_key_seed.len() != 32 {
        return Err(js_error("call_key_seed must be 32 bytes"));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(call_key_seed);
    let ck = lakko_calls::CallKey::from_seed(seed);
    let pk = ck
        .participant_key_for(sender_leaf, stream_index)
        .map_err(calls_err)?;
    Ok(SframeParticipantKey {
        key_bytes_val: pk.key_bytes().to_vec(),
        nonce_salt_val: pk.nonce_salt().to_vec(),
    })
}

/// Open an SFrame frame with the Phase 3c5 overlap window.
/// Tries `currentCallKeySeed` first; on AEAD rejection
/// falls back to `previousCallKeySeed` if not `null`
/// (caller passes `null` outside the 1-second overlap
/// window). Returns the plaintext on success.
///
/// Seed-flat shape — platform code holds the 32-byte seeds
/// in its own non-extractable WebCrypto store and ferries
/// them in per-call. `kid` is the composite SFrame key identifier
/// `(sender_leaf << 8) | stream_index` (ADR-0035) the receiver read from the
/// frame's header (crossing to JS as a `BigInt`); the decryptor re-derives the
/// exact sender-stream key from the KID with no separate media discriminator.
#[wasm_bindgen(js_name = sframeOpenWithOverlap)]
pub fn sframe_open_with_overlap(
    current_call_key_seed: &[u8],
    previous_call_key_seed: Option<Vec<u8>>,
    kid: u64,
    aad: &[u8],
    ciphertext: &[u8],
    frame_counter: u32,
) -> Result<Vec<u8>, JsValue> {
    let take_seed = |bytes: &[u8]| -> Result<[u8; 32], JsValue> {
        if bytes.len() != 32 {
            return Err(js_error(&format!(
                "call_key_seed must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(bytes);
        Ok(seed)
    };

    let current_seed = take_seed(current_call_key_seed)?;
    let current = lakko_calls::CallKey::from_seed(current_seed);
    let current_key = current.participant_key(kid).map_err(calls_err)?;
    match current_key.open(aad, ciphertext, frame_counter) {
        Ok(pt) => return Ok(pt),
        Err(lakko_calls::Error::Crypto(_)) => {
            // Fall through to the overlap-window path.
        },
        Err(other) => return Err(calls_err(other)),
    }

    if let Some(prev_bytes) = previous_call_key_seed {
        let prev_seed = take_seed(&prev_bytes)?;
        let previous = lakko_calls::CallKey::from_seed(prev_seed);
        let prev_key = previous.participant_key(kid).map_err(calls_err)?;
        return prev_key
            .open(aad, ciphertext, frame_counter)
            .map_err(calls_err);
    }
    Err(js_error(
        "frame failed authentication under current key (no overlap window active)",
    ))
}

// ============================================================================
// Phase 3b4 — Call-admission JWT FFI surface (web client)
// ============================================================================
//
// Same shape Swift / Kotlin get via UniFFI's
// `FfiCallJwtClaims` Record; here we expose a `ParsedCallJwt`
// class with getters because wasm-bindgen rejects enums with
// associated data (same constraint that drove the
// `ParsedCallSignaling` flat-class shape in 3a4 / 3c2).

/// Parsed call-JWT claims — flat class with getters.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct ParsedCallJwt {
    iss_val: String,
    aud_val: String,
    sub_val: String,
    call_id_val: String,
    role_val: &'static str,
    iat_val: u64,
    exp_val: u64,
    kid_val: String,
}

fn role_str(role: lakko_call_jwt::CallRole) -> &'static str {
    match role {
        lakko_call_jwt::CallRole::Participant => "participant",
        lakko_call_jwt::CallRole::Publisher => "publisher",
        lakko_call_jwt::CallRole::Subscriber => "subscriber",
    }
}

fn claims_to_parsed(claims: lakko_call_jwt::CallJwtClaims, kid: String) -> ParsedCallJwt {
    ParsedCallJwt {
        iss_val: claims.iss,
        aud_val: claims.aud,
        sub_val: claims.sub,
        call_id_val: claims.call_id,
        role_val: role_str(claims.role),
        iat_val: claims.iat,
        exp_val: claims.exp,
        kid_val: kid,
    }
}

fn jwt_err(e: lakko_call_jwt::JwtError) -> JsValue {
    js_error(&format!("{e}"))
}

#[wasm_bindgen]
impl ParsedCallJwt {
    /// `iss` claim — always `"lakko-auth"` on a well-formed JWT.
    #[wasm_bindgen(getter)]
    pub fn iss(&self) -> String {
        self.iss_val.clone()
    }
    /// `aud` claim — always `"lakko-sfu"`.
    #[wasm_bindgen(getter)]
    pub fn aud(&self) -> String {
        self.aud_val.clone()
    }
    /// account_id, 32-char lower-case hex.
    #[wasm_bindgen(getter)]
    pub fn sub(&self) -> String {
        self.sub_val.clone()
    }
    /// call_id, 32-char lower-case hex.
    #[wasm_bindgen(getter, js_name = callId)]
    pub fn call_id(&self) -> String {
        self.call_id_val.clone()
    }
    /// One of `"participant"` / `"publisher"` / `"subscriber"`.
    #[wasm_bindgen(getter)]
    pub fn role(&self) -> String {
        self.role_val.to_string()
    }
    /// `iat` — issued-at, unix seconds.
    #[wasm_bindgen(getter)]
    pub fn iat(&self) -> u64 {
        self.iat_val
    }
    /// `exp` — expires-at, unix seconds.
    #[wasm_bindgen(getter)]
    pub fn exp(&self) -> u64 {
        self.exp_val
    }
    /// JWS header `kid` — 16 hex chars.
    #[wasm_bindgen(getter)]
    pub fn kid(&self) -> String {
        self.kid_val.clone()
    }
}

/// Parse a JWT into typed claims WITHOUT verifying the
/// signature. Useful for inspecting role / expiry / kid
/// client-side before presenting the JWT to the SFU.
#[wasm_bindgen(js_name = parseCallJwtUnverified)]
pub fn parse_call_jwt_unverified(jwt: &str) -> Result<ParsedCallJwt, JsValue> {
    let (header, claims) = lakko_call_jwt::parse_unverified(jwt).map_err(jwt_err)?;
    Ok(claims_to_parsed(claims, header.kid))
}

/// Verify a JWT against a JWKS document the client fetched
/// from `GET /.well-known/jwks.json`. Returns the typed
/// claims on success.
///
/// `nowUnixSecs` is injected so the platform controls the
/// clock source — pass `Math.floor(Date.now() / 1000)`.
#[wasm_bindgen(js_name = verifyCallJwtWithJwks)]
pub fn verify_call_jwt_with_jwks(
    jwks_doc_bytes: &[u8],
    jwt: &str,
    now_unix_secs: u64,
) -> Result<ParsedCallJwt, JsValue> {
    let jwks = lakko_call_jwt::parse_jwks(jwks_doc_bytes).map_err(jwt_err)?;
    let claims = lakko_call_jwt::verify_with_jwks(&jwks, jwt, now_unix_secs).map_err(jwt_err)?;
    let (header, _) = lakko_call_jwt::parse_unverified(jwt).map_err(jwt_err)?;
    Ok(claims_to_parsed(claims, header.kid))
}

// ============================================================================
// Phase 3a11 — Call quality metrics FFI (web client)
// ============================================================================

/// MOS estimation result. `level` is one of
/// `"excellent" | "good" | "fair" | "poor" | "bad"`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct CallQualityScore {
    r_factor_val: f32,
    mos_val: f32,
    level_val: &'static str,
}

#[wasm_bindgen]
impl CallQualityScore {
    /// E-model R-factor in `[0.0, 100.0]`.
    #[wasm_bindgen(getter, js_name = rFactor)]
    pub fn r_factor(&self) -> f32 {
        self.r_factor_val
    }

    /// Mean Opinion Score in `[1.0, 4.5]`.
    #[wasm_bindgen(getter)]
    pub fn mos(&self) -> f32 {
        self.mos_val
    }

    /// One of `"excellent"` / `"good"` / `"fair"` /
    /// `"poor"` / `"bad"`.
    #[wasm_bindgen(getter)]
    pub fn level(&self) -> String {
        self.level_val.to_string()
    }
}

fn level_str(l: lakko_calls::QualityLevel) -> &'static str {
    match l {
        lakko_calls::QualityLevel::Excellent => "excellent",
        lakko_calls::QualityLevel::Good => "good",
        lakko_calls::QualityLevel::Fair => "fair",
        lakko_calls::QualityLevel::Poor => "poor",
        lakko_calls::QualityLevel::Bad => "bad",
    }
}

fn parse_codec(s: &str) -> lakko_calls::AudioCodec {
    match s {
        "opus" | "Opus" | "OPUS" => lakko_calls::AudioCodec::Opus,
        _ => lakko_calls::AudioCodec::Other,
    }
}

/// Estimate call quality from a single network sample
/// (Phase 3a11). `codec` accepts `"opus"` (default for
/// Lakko voice / video) or any other value (mapped to the
/// no-FEC baseline).
#[wasm_bindgen(js_name = estimateCallQuality)]
pub fn estimate_call_quality(
    rtt_ms: u32,
    jitter_ms: u32,
    loss_pct: f32,
    codec: &str,
) -> CallQualityScore {
    let sample = lakko_calls::QualitySample {
        rtt_ms,
        jitter_ms,
        loss_pct,
        codec: parse_codec(codec),
    };
    let score = lakko_calls::estimate_quality(sample);
    CallQualityScore {
        r_factor_val: score.r_factor,
        mos_val: score.mos,
        level_val: level_str(score.level),
    }
}

/// Recommend a new encoder bitrate given the current one +
/// a fresh quality sample. Returns the current value
/// unchanged when the sample falls in the "hold" band.
#[wasm_bindgen(js_name = recommendCallBitrate)]
pub fn recommend_call_bitrate(
    current_bps: u32,
    rtt_ms: u32,
    jitter_ms: u32,
    loss_pct: f32,
    codec: &str,
) -> u32 {
    let sample = lakko_calls::QualitySample {
        rtt_ms,
        jitter_ms,
        loss_pct,
        codec: parse_codec(codec),
    };
    lakko_calls::recommend_bitrate(current_bps, sample)
}

/// Default ring-buffer size for [`CallQualitySampler`].
#[wasm_bindgen(js_name = defaultQualitySampleWindow)]
#[must_use]
pub fn default_quality_sample_window() -> u32 {
    u32::try_from(lakko_calls::DEFAULT_SAMPLE_WINDOW).unwrap_or(5)
}

/// Maximum ring-buffer size accepted by
/// [`CallQualitySampler::new`].
#[wasm_bindgen(js_name = maxQualitySampleWindow)]
#[must_use]
pub fn max_quality_sample_window() -> u32 {
    u32::try_from(lakko_calls::MAX_SAMPLE_WINDOW).unwrap_or(60)
}

/// Stateful rolling-window smoothed quality sampler
/// (Phase 3a11). One instance per active call.
///
/// `record` pushes a fresh sample and returns the
/// smoothed [`CallQualityScore`]; `latest` returns the
/// current smoothed score without pushing; `clear` resets
/// the window (on call end / SFU handoff).
#[wasm_bindgen]
pub struct CallQualitySampler {
    inner: lakko_calls::QualitySampler,
}

#[wasm_bindgen]
impl CallQualitySampler {
    /// Construct a sampler with a custom window size.
    /// Sizes outside `[1, MAX_SAMPLE_WINDOW]` are clamped.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(window_size: u32) -> Self {
        let bounded = usize::try_from(window_size).unwrap_or(lakko_calls::DEFAULT_SAMPLE_WINDOW);
        Self {
            inner: lakko_calls::QualitySampler::new(bounded),
        }
    }

    /// Effective window size after clamping.
    #[wasm_bindgen(getter, js_name = windowSize)]
    pub fn window_size(&self) -> u32 {
        u32::try_from(self.inner.window_size()).unwrap_or(u32::MAX)
    }

    /// Number of samples currently buffered.
    #[wasm_bindgen(getter)]
    pub fn count(&self) -> u32 {
        u32::try_from(self.inner.count()).unwrap_or(u32::MAX)
    }

    /// Drop every buffered sample.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Push a sample and return the smoothed score.
    pub fn record(
        &mut self,
        rtt_ms: u32,
        jitter_ms: u32,
        loss_pct: f32,
        codec: &str,
    ) -> CallQualityScore {
        let sample = lakko_calls::QualitySample {
            rtt_ms,
            jitter_ms,
            loss_pct,
            codec: parse_codec(codec),
        };
        let score = self.inner.record(sample);
        CallQualityScore {
            r_factor_val: score.r_factor,
            mos_val: score.mos,
            level_val: level_str(score.level),
        }
    }

    /// Latest smoothed score without pushing a new sample.
    /// Returns `null` when the sampler is empty.
    pub fn latest(&self) -> Option<CallQualityScore> {
        self.inner.latest().map(|score| CallQualityScore {
            r_factor_val: score.r_factor,
            mos_val: score.mos,
            level_val: level_str(score.level),
        })
    }
}

// ============================================================================
// Phase 3c3-joiner — link-based call invite acceptance FFI (web)
// ============================================================================

/// Result of [`acceptCallInvite`]. Carries the fresh MLS
/// group, the Welcome bytes for the owner, the call_id,
/// and the call-key seed. The group is exposed as a
/// `Group` so the platform can continue MLS ops via the
/// existing wasm-bindgen surface; the seed is flat so it
/// feeds straight into `sframeDeriveParticipantKey`.
#[wasm_bindgen]
pub struct AcceptedCallInvite {
    group_field: Group,
    welcome_for_owner_val: Vec<u8>,
    call_id_val: Vec<u8>,
    own_leaf_index_val: u32,
    call_key_seed_val: Vec<u8>,
}

#[wasm_bindgen]
impl AcceptedCallInvite {
    /// Take ownership of the freshly-created MLS group
    /// (joiner is founder at leaf 0). Wasm-bindgen one-
    /// shot move — calling this twice panics; the
    /// platform must capture the result on first call.
    pub fn group(&mut self) -> Group {
        let placeholder = Group {
            inner: std::sync::Mutex::new(
                MlsGroup::create(
                    &lakko_mls::Identity::generate("placeholder").expect("placeholder identity"),
                )
                .expect("placeholder group"),
            ),
        };
        std::mem::replace(&mut self.group_field, placeholder)
    }

    /// MLS wire-format Welcome bytes for the owner to
    /// run `Group.joinFromWelcome` against. Platform
    /// delivers these out-of-band.
    #[wasm_bindgen(getter, js_name = welcomeForOwner)]
    pub fn welcome_for_owner(&self) -> Vec<u8> {
        self.welcome_for_owner_val.clone()
    }

    /// 16-byte `call_id` from the capability.
    #[wasm_bindgen(getter, js_name = callId)]
    pub fn call_id(&self) -> Vec<u8> {
        self.call_id_val.clone()
    }

    /// Joiner's leaf index — always 0 for the founder.
    #[wasm_bindgen(getter, js_name = ownLeafIndex)]
    pub fn own_leaf_index(&self) -> u32 {
        self.own_leaf_index_val
    }

    /// 32-byte `CallKey` seed. Feed into
    /// `sframeDeriveParticipantKey` to get the per-leaf
    /// SFrame key.
    #[wasm_bindgen(getter, js_name = callKeySeed)]
    pub fn call_key_seed(&self) -> Vec<u8> {
        self.call_key_seed_val.clone()
    }
}

/// Accept a verified call-invite capability (Phase 3c3).
/// `capability` must already be `decodeAndVerify`'d.
#[wasm_bindgen(js_name = acceptCallInvite)]
pub fn accept_call_invite(
    capability: &crate::contacts::CallInviteCapability,
    joiner_identity: &Identity,
) -> Result<AcceptedCallInvite, JsValue> {
    let accepted = lakko_calls::accept_call_invite(&capability.inner, &joiner_identity.inner)
        .map_err(|e| js_error(&format!("{e}")))?;
    let call_key_seed = accepted.session.call_key().as_bytes().to_vec();
    let call_id = accepted.session.call_id.to_vec();
    let own_leaf_index = accepted.session.own_leaf_index;
    Ok(AcceptedCallInvite {
        group_field: Group {
            inner: Mutex::new(accepted.group),
        },
        welcome_for_owner_val: accepted.welcome_for_owner,
        call_id_val: call_id,
        own_leaf_index_val: own_leaf_index,
        call_key_seed_val: call_key_seed,
    })
}

/// Seal `plaintext` under a raw 32-byte AEAD `key` (ADR-0018 Part 2a, factor
/// (a) — "another-device" identity transfer, Path B). Returns the self-
/// describing wire blob `nonce(12) ‖ ciphertext+tag` (ChaCha20-Poly1305, empty
/// AAD; the nonce is generated internally and prepended). The existing device
/// derives `key` via `deviceGroup.exportSecret(...)` and seals
/// `(grant_token ‖ phrase_seed)` here, then publishes the blob to the
/// rendezvous mailbox; the fresh device derives the same `key` and opens it
/// via [`open_with_key`]. Mirror of the UniFFI `seal_with_key`.
#[wasm_bindgen(js_name = sealWithKey)]
pub fn seal_with_key(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, JsValue> {
    let key = key_32(key)?;
    lakko_crypto::aead::seal_with_key(&key, plaintext)
        .map_err(|e| js_error(&format!("sealWithKey: {e}")))
}

/// Inverse of [`seal_with_key`]: open a `nonce(12) ‖ ciphertext+tag` wire blob
/// under the same raw 32-byte `key`. Returns the recovered plaintext. Throws
/// when the key is not 32 bytes, the blob is too short, or authentication
/// fails (wrong key / tampered blob).
#[wasm_bindgen(js_name = openWithKey)]
pub fn open_with_key(key: &[u8], wire: &[u8]) -> Result<Vec<u8>, JsValue> {
    let key = key_32(key)?;
    lakko_crypto::aead::open_with_key(&key, wire)
        .map_err(|e| js_error(&format!("openWithKey: {e}")))
}

/// Validate + copy a caller-supplied key into the fixed 32-byte array
/// `lakko_crypto::aead::{seal_with_key, open_with_key}` require.
fn key_32(key: &[u8]) -> Result<[u8; 32], JsValue> {
    key.try_into()
        .map_err(|_| js_error(&format!("key must be 32 bytes, got {}", key.len())))
}

// Tests run on the host target (`cargo test`) — no `wasm-bindgen-test`
// runner needed for what we cover here. The wire-level invariants
// from `lakko-mls` are unchanged regardless of FFI binding; this
// suite exists to catch shape regressions in the wasm-bindgen
// surface itself (constructor signatures, getter return types, the
// ProcessedMessage tagging convention).

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of the lakko-mls integration test driven through the
    /// wasm-bindgen surface. Same end-to-end check the UniFFI mirror
    /// runs — exercises identity creation, group creation, member
    /// addition (commit + welcome split), application-message round-
    /// trip, exporter consistency.
    #[test]
    fn wasm_three_party_lifecycle() {
        let alice = Identity::new("alice").expect("alice identity");
        let bob = Identity::new("bob").expect("bob identity");
        let charlie = Identity::new("charlie").expect("charlie identity");

        let alice_group = Group::new(&alice).expect("alice group");

        // Alice adds Bob.
        let bob_kp = bob.create_key_package().expect("bob key package");
        let add_bob = alice_group.add_member(&alice, &bob_kp).expect("add bob");
        let bob_group = Group::join_from_welcome(&add_bob.welcome(), &bob).expect("bob join");

        // Alice → Bob application message.
        let msg = alice_group
            .create_application_message(&alice, b"hello via wasm")
            .expect("alice send");
        let received = bob_group.process_incoming(&bob, &msg).expect("bob recv");
        assert_eq!(received.kind(), "Application");
        assert_eq!(
            received.application_data().as_deref(),
            Some(&b"hello via wasm"[..]),
        );

        // Alice adds Charlie; Bob processes the commit.
        let charlie_kp = charlie.create_key_package().expect("charlie kp");
        let add_charlie = alice_group
            .add_member(&alice, &charlie_kp)
            .expect("add charlie");
        let bob_post = bob_group
            .process_incoming(&bob, &add_charlie.commit())
            .expect("bob process commit");
        assert_eq!(bob_post.kind(), "Commit");
        assert!(bob_post.application_data().is_none());

        let charlie_group =
            Group::join_from_welcome(&add_charlie.welcome(), &charlie).expect("charlie join");

        // Member counts converge.
        assert_eq!(alice_group.member_count().unwrap(), 3);
        assert_eq!(bob_group.member_count().unwrap(), 3);
        assert_eq!(charlie_group.member_count().unwrap(), 3);

        // Exporter consistency — identical secrets across the group.
        let alice_export = alice_group
            .export_secret(&alice, "lakko/test", b"ctx", 32)
            .expect("alice export");
        let charlie_export = charlie_group
            .export_secret(&charlie, "lakko/test", b"ctx", 32)
            .expect("charlie export");
        assert_eq!(alice_export, charlie_export);
    }

    // ========================================================================
    // ADR-0045 — proposal + sweep + rank surface
    // ========================================================================

    /// Set up `alice` (owner) + `bob` + `charlie` in one group.
    fn adr0045_trio() -> ((Identity, Group), (Identity, Group), (Identity, Group)) {
        let alice = Identity::new("alice").unwrap();
        let bob = Identity::new("bob").unwrap();
        let charlie = Identity::new("charlie").unwrap();

        let alice_g = Group::new(&alice).unwrap();
        let add_bob = alice_g
            .add_member(&alice, &bob.create_key_package().unwrap())
            .unwrap();
        let bob_g = Group::join_from_welcome(&add_bob.welcome(), &bob).unwrap();

        let add_charlie = alice_g
            .add_member(&alice, &charlie.create_key_package().unwrap())
            .unwrap();
        assert_eq!(
            bob_g
                .process_incoming_verifying_roles(&bob, &add_charlie.commit())
                .unwrap()
                .kind(),
            "Commit"
        );
        let charlie_g = Group::join_from_welcome(&add_charlie.welcome(), &charlie).unwrap();

        ((alice, alice_g), (bob, bob_g), (charlie, charlie_g))
    }

    /// ADR-0045 **S7** across the wasm-bindgen boundary: `approveExternalJoin`
    /// hands back a **Proposal**, not a Commit, and the Welcome the joiner
    /// needs comes out of the sweep instead.
    ///
    /// This is the shape the web client is built from, so it is asserted on the
    /// wasm types rather than inferred from the core.
    ///
    /// Only the success path is exercised: the wasm-bindgen `JsError`
    /// constructor panics under `cargo test` (off-wasm target), so the
    /// plain-member refusal is pinned in `lakko-mls` and in the UniFFI mirror
    /// instead — the same split the rest of this suite already lives with.
    #[test]
    fn approving_an_external_join_yields_a_proposal_and_the_sweep_yields_the_welcome() {
        let ((alice, alice_g), (bob, bob_g), _charlie) = adr0045_trio();
        let dave = Identity::new("dave").unwrap();

        // A stranger asks, against the group's current epoch.
        let request = dave
            .create_external_join_proposal(&alice_g.group_id().unwrap(), alice_g.epoch().unwrap())
            .unwrap();
        let seen = alice_g
            .process_incoming_verifying_roles(&alice, &request)
            .unwrap();
        assert_eq!(seen.kind(), "ExternalJoinProposal");
        let joiner_key = seen
            .external_join_signature_key()
            .expect("the joiner's signature key");
        assert_eq!(alice_g.pending_external_joins().unwrap(), joiner_key);

        // The approve is a proposal: bytes to publish, and no epoch advance.
        let epoch_before = alice_g.epoch().unwrap();
        let approval = alice_g.approve_external_join(&alice, &joiner_key).unwrap();
        assert!(!approval.is_empty());
        assert_eq!(alice_g.epoch().unwrap(), epoch_before);
        assert_eq!(alice_g.pending_proposals_core().unwrap().len(), 1);

        // On the wire it is an ordinary admin-authored Add: bob's receipt gate
        // stores it and cannot tell it came from a `/g/` link.
        let received = bob_g
            .process_incoming_verifying_roles(&bob, &approval)
            .unwrap();
        assert_eq!(received.kind(), "Proposal");
        let record = received.proposal().expect("kind Proposal carries a record");
        assert!(record.stored());
        assert_eq!(record.kind(), "add");

        // The sweep is what produces the Welcome.
        let swept = alice_g
            .commit_pending_proposals_core(&alice)
            .unwrap()
            .expect("a commit was produced");
        assert_eq!(swept.swept_records.len(), 1);
        let welcome = swept
            .welcome_bytes
            .clone()
            .expect("an add sweep carries a welcome");
        let dave_g = Group::join_from_welcome(&welcome, &dave).unwrap();
        assert_eq!(dave_g.epoch().unwrap(), epoch_before + 1);
        assert!(alice_g.pending_external_joins().unwrap().is_empty());
    }

    /// ADR-0045 end to end across the wasm-bindgen boundary: the owner
    /// **proposes** an add (epoch unchanged), a peer receives it as a stored
    /// `ProposalRecord`, the owner sweeps it into one commit, and the peer
    /// merges.
    #[test]
    fn propose_then_sweep_round_trips_through_the_wasm_types() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let dave = Identity::new("dave").unwrap();
        let dave_key = key_package_signature_key(&dave.create_key_package().unwrap()).unwrap();

        let epoch_before = alice_g.epoch().unwrap();
        let proposal = alice_g
            .propose_add_member_core(&alice, &dave.create_key_package().unwrap())
            .unwrap();
        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "a proposal must not advance the epoch",
        );
        assert_eq!(alice_g.pending_proposals_core().unwrap().len(), 1);

        // Bob receives it through the role-verifying gate: authorised (the
        // proposer is the owner) and therefore STORED.
        let received = bob_g
            .process_incoming_verifying_roles(&bob, &proposal)
            .unwrap();
        assert_eq!(received.kind(), "Proposal");
        let record = received.proposal().expect("kind Proposal carries a record");
        assert!(record.stored());
        assert_eq!(record.kind(), "add");
        assert_eq!(
            record.sender_leaf(),
            Some(alice_g.own_leaf_index().unwrap())
        );
        assert_eq!(record.add_signature_key(), Some(dave_key));
        assert_eq!(bob_g.pending_proposals_core().unwrap().len(), 1);

        // Charlie receives the SAME bytes through the OPEN path, which has no
        // role table to judge against and so stores nothing — reporting the
        // drop as data rather than as an error.
        let charlie_saw = charlie_g.process_incoming(&charlie, &proposal).unwrap();
        let dropped = charlie_saw.proposal().expect("record present");
        assert!(
            !dropped.stored(),
            "the open receive path must report the proposal as dropped",
        );
        assert_eq!(dropped.kind(), "add");
        assert!(charlie_g.pending_proposals_core().unwrap().is_empty());

        // Alice sweeps.
        let swept = alice_g
            .commit_pending_proposals_core(&alice)
            .unwrap()
            .expect("a pending add must produce a commit");
        assert_eq!(swept.swept_records.len(), 1);
        let welcome = swept
            .welcome_bytes
            .clone()
            .expect("an add sweep carries a welcome");
        assert!(alice_g.epoch().unwrap() > epoch_before);

        assert_eq!(
            bob_g
                .process_incoming_verifying_roles(&bob, &swept.commit_bytes)
                .unwrap()
                .kind(),
            "Commit",
        );
        assert!(bob_g.pending_proposals_core().unwrap().is_empty());
        let dave_g = Group::join_from_welcome(&welcome, &dave).unwrap();
        assert_eq!(alice_g.member_count().unwrap(), 4);
        assert_eq!(dave_g.member_count().unwrap(), 4);

        // Nothing left to sweep => `undefined`, NOT an empty commit.
        assert!(alice_g
            .commit_pending_proposals_core(&alice)
            .unwrap()
            .is_none());
    }

    /// `isActive` crosses the wasm-bindgen boundary and tracks the eviction.
    ///
    /// The core property — that the accessor and the roster inference never
    /// disagree — is pinned in `lakko-mls`'s `group_membership_state` suite;
    /// what this asserts is the **wiring**, because the failure mode of a
    /// wrongly-plumbed accessor is a composer that stays enabled on a dead
    /// group, which is exactly the gap it exists to close.
    #[test]
    fn is_active_tracks_an_eviction_across_the_ffi_boundary() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();

        assert!(alice_g.is_active().unwrap(), "the owner starts active");
        assert!(charlie_g.is_active().unwrap(), "a member starts active");

        // Charlie leaves: a member may always propose its own removal. It does
        // not re-process its own proposal — OpenMLS refuses an own message, and
        // `proposeRemoveMember` already queued it locally.
        let charlie_leaf = charlie_g.own_leaf_index().unwrap();
        let leave = charlie_g
            .propose_remove_member_core(&charlie, charlie_leaf)
            .unwrap();
        alice_g
            .process_incoming_verifying_roles(&alice, &leave)
            .unwrap();
        bob_g
            .process_incoming_verifying_roles(&bob, &leave)
            .unwrap();

        let sweep = alice_g
            .commit_pending_proposals_core(&alice)
            .unwrap()
            .expect("a pending removal must produce a commit");

        charlie_g
            .process_incoming_verifying_roles(&charlie, &sweep.commit_bytes)
            .unwrap();
        bob_g
            .process_incoming_verifying_roles(&bob, &sweep.commit_bytes)
            .unwrap();

        assert!(
            !charlie_g.is_active().unwrap(),
            "the evicted device must read inactive through the FFI",
        );
        assert!(
            alice_g.is_active().unwrap() && bob_g.is_active().unwrap(),
            "the survivors must stay active",
        );
        // The asymmetry the UI depends on: reads still answer on a dead group.
        // (The matching "and sends fail" half is asserted in `lakko-mls` and on
        // the UniFFI surface — here a `JsError` cannot be constructed off-wasm,
        // which is why the sibling tests reach for the `_core` variants.)
        assert_eq!(charlie_g.member_count().unwrap(), 2);
    }

    /// The wasm flattening puts each variant's payload behind its **own**
    /// getter, so a `remove`'s target leaf can never be read as an `add`'s
    /// signature key and vice versa. This is the property that replaces the
    /// type-level discrimination UniFFI gets for free from its enum.
    #[test]
    fn add_and_remove_payload_getters_are_disjoint() {
        let ((alice, alice_g), (bob, bob_g), _charlie) = adr0045_trio();
        let dave = Identity::new("dave").unwrap();
        let dave_key = key_package_signature_key(&dave.create_key_package().unwrap()).unwrap();

        let add = alice_g
            .propose_add_member_core(&alice, &dave.create_key_package().unwrap())
            .unwrap();
        let add_rec = bob_g
            .process_incoming_verifying_roles(&bob, &add)
            .unwrap()
            .proposal()
            .expect("record");
        assert_eq!(add_rec.kind(), "add");
        assert_eq!(add_rec.add_signature_key(), Some(dave_key));
        assert_eq!(
            add_rec.remove_target_leaf(),
            None,
            "an add must expose NO remove target",
        );

        // Bob proposes his OWN removal — leaving, which needs no role.
        let bob_leaf = bob_g.own_leaf_index().unwrap();
        let leave = bob_g.propose_remove_member_core(&bob, bob_leaf).unwrap();
        let rm_rec = alice_g
            .process_incoming_verifying_roles(&alice, &leave)
            .unwrap()
            .proposal()
            .expect("record");
        assert_eq!(rm_rec.kind(), "remove");
        assert!(rm_rec.stored());
        assert_eq!(rm_rec.remove_target_leaf(), Some(bob_leaf));
        assert_eq!(
            rm_rec.add_signature_key(),
            None,
            "a remove must expose NO add signature key",
        );
    }

    /// The committer rank must distinguish "owner" (`Some(0)` / JS `0`) from
    /// "not in the committer set" (`None` / JS `undefined`). Collapsing the
    /// absent case into `0` would make every plain member believe it is the
    /// owner and sweep.
    #[test]
    fn committer_rank_never_conflates_absent_with_rank_zero() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();

        assert_eq!(alice_g.own_committer_rank_core(&alice).unwrap(), Some(0));
        assert_eq!(
            bob_g.own_committer_rank_core(&bob).unwrap(),
            None,
            "a plain member has NO rank — not rank 0",
        );

        let promote = alice_g
            .set_group_roles(
                &alice,
                &alice.signature_public_key(),
                &bob.signature_public_key(),
            )
            .unwrap();
        for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
            assert_eq!(
                g.process_incoming_verifying_roles(id, &promote)
                    .unwrap()
                    .kind(),
                "Commit"
            );
        }
        assert_eq!(alice_g.own_committer_rank_core(&alice).unwrap(), Some(0));
        assert_eq!(bob_g.own_committer_rank_core(&bob).unwrap(), Some(1));
        assert_eq!(
            charlie_g.own_committer_rank_core(&charlie).unwrap(),
            None,
            "a plain member stays absent while an admin gains a rank",
        );
    }

    /// A plain member's unauthorised proposal is refused **locally**, before
    /// anything reaches the wire.
    #[test]
    fn a_plain_member_cannot_mint_an_unauthorised_proposal() {
        let ((alice, alice_g), _bob, (charlie, charlie_g)) = adr0045_trio();
        let dave = Identity::new("dave").unwrap();

        assert!(charlie_g
            .propose_add_member_core(&charlie, &dave.create_key_package().unwrap())
            .is_err());
        assert!(charlie_g
            .propose_remove_member_core(&charlie, alice_g.own_leaf_index().unwrap())
            .is_err());
        assert!(charlie_g
            .propose_group_roles_core(
                &charlie,
                &charlie.signature_public_key(),
                &alice.signature_public_key(),
            )
            .is_err());
        assert!(charlie_g.commit_pending_proposals_core(&charlie).is_err());
        assert!(charlie_g.pending_proposals_core().unwrap().is_empty());
    }

    // ========================================================================
    // ADR-0045 S3b — the staged sweep + ownership transfer
    // ========================================================================

    /// **Won.** Stage leaves the group at the epoch it built against, holding
    /// an un-merged commit; the verdict merge lands it at the next epoch, in
    /// the same state the peer that applied the published commit reached.
    ///
    /// This is the sequence the whole staged pair exists for: stage, publish,
    /// read the journal order, merge.
    #[test]
    fn staging_then_merging_lands_the_sweep() {
        let ((alice, alice_g), (bob, bob_g), _charlie) = adr0045_trio();
        let dave = Identity::new("dave").unwrap();

        // The proposal reaches every member FIRST. A swept commit carries it
        // by REFERENCE, and OpenMLS resolves those references at commit
        // receipt on every member — a peer that never saw the proposal refuses
        // the commit with `InvalidCommit(MissingProposal)`. That is RFC 9420
        // enforcing ADR-0045's "store at receipt" rule, not a test artefact.
        let proposal = alice_g
            .propose_add_member_core(&alice, &dave.create_key_package().unwrap())
            .unwrap();
        assert_eq!(
            bob_g
                .process_incoming_verifying_roles(&bob, &proposal)
                .unwrap()
                .kind(),
            "Proposal",
        );
        let epoch_before = alice_g.epoch().unwrap();
        assert!(!alice_g.has_staged_sweep_core().unwrap());

        // 1. Stage. The epoch does NOT move, and the group now owes a verdict.
        let staged = alice_g
            .stage_pending_proposals_core(&alice)
            .unwrap()
            .expect("a pending add must produce a staged sweep");
        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "staging must NOT advance the epoch — that is the whole point",
        );
        assert!(
            alice_g.has_staged_sweep_core().unwrap(),
            "a staged sweep must be reported as awaiting a verdict",
        );
        assert_eq!(staged.swept_records.len(), 1);
        assert_eq!(staged.swept_records[0].kind(), "add");
        let welcome = staged
            .welcome_bytes
            .clone()
            .expect("an add sweep carries a welcome");

        // 2. Publish. Bob applies it and reaches the new epoch alone.
        assert_eq!(
            bob_g
                .process_incoming_verifying_roles(&bob, &staged.commit_bytes)
                .unwrap()
                .kind(),
            "Commit",
        );
        assert_eq!(
            bob_g.epoch().unwrap(),
            epoch_before + 1,
            "the peer advances on the published commit while the stager has not",
        );

        // 3 + 4. The journal ordered it first, so merge.
        assert_eq!(
            alice_g.merge_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::Merged,
        );
        assert!(!alice_g.has_staged_sweep_core().unwrap());
        assert_eq!(alice_g.epoch().unwrap(), bob_g.epoch().unwrap());
        assert!(
            alice_g.pending_proposals_core().unwrap().is_empty(),
            "a merged sweep empties the proposal store",
        );

        let dave_g = Group::join_from_welcome(&welcome, &dave).unwrap();
        assert_eq!(dave_g.member_count().unwrap(), 4);
        assert_eq!(alice_g.member_count().unwrap(), 4);

        // Merging again is an idempotent no-op — what makes ADR-0040's restart
        // sweep safe to run twice.
        assert_eq!(
            alice_g.merge_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
        );
    }

    /// **Lost.** Abandoning returns the group to the epoch the sweep was built
    /// against and throws no work away — the proposals it selected are still
    /// pending, so the winner (or a later sweep) picks them up.
    #[test]
    fn staging_then_abandoning_rolls_back_and_keeps_the_proposals() {
        let ((alice, alice_g), _bob, _charlie) = adr0045_trio();
        let dave = Identity::new("dave").unwrap();

        alice_g
            .propose_add_member_core(&alice, &dave.create_key_package().unwrap())
            .unwrap();
        let epoch_before = alice_g.epoch().unwrap();
        let members_before = alice_g.member_count().unwrap();

        let staged = alice_g
            .stage_pending_proposals_core(&alice)
            .unwrap()
            .unwrap();
        assert_eq!(staged.swept_records.len(), 1);
        assert!(alice_g.has_staged_sweep_core().unwrap());

        assert_eq!(
            alice_g.abandon_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::Abandoned,
        );
        assert!(!alice_g.has_staged_sweep_core().unwrap());
        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "abandoning must leave the group exactly where it was",
        );
        assert_eq!(alice_g.member_count().unwrap(), members_before);
        assert_eq!(
            alice_g.pending_proposals_core().unwrap().len(),
            1,
            "an abandoned sweep leaves its proposals for the next committer",
        );

        // And the group is free to act again: a second stage now succeeds.
        let restaged = alice_g
            .stage_pending_proposals_core(&alice)
            .unwrap()
            .unwrap();
        assert_eq!(restaged.swept_records.len(), 1);
        assert_eq!(
            alice_g.merge_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::Merged,
        );
        assert_eq!(alice_g.member_count().unwrap(), members_before + 1);

        // Abandoning again is an idempotent no-op.
        assert_eq!(
            alice_g.abandon_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
        );
    }

    // ========================================================================
    // Typed membership refusals (ADR-0045 §9.1 item 1 / §12 item 6)
    // ========================================================================

    /// Every core [`lakko_mls::RefusalReason`] reaches the JS boundary with its
    /// reason and its sentence intact.
    ///
    /// The final hop — attaching `refusalReason` / `refusalClass` to the thrown
    /// `Error` — is `js_sys` and cannot run off wasm32, so it is asserted in
    /// `wasm_tests.rs`. Everything up to it is asserted here, which is where a
    /// mapping mistake would actually live.
    #[test]
    fn every_refusal_reason_survives_the_projection_with_its_sentence() {
        let mut codes: Vec<&str> = Vec::new();
        for reason in lakko_mls::RefusalReason::ALL {
            let f = MlsFailure::from(lakko_mls::Error::refused_for_tests(
                reason,
                "the sentence the core wrote",
            ));
            assert_eq!(f.reason, Some(reason), "{reason:?} lost its reason");
            assert_eq!(
                f.message, "the sentence the core wrote",
                "{reason:?} lost its sentence",
            );
            codes.push(reason.code());
        }
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "refusal codes must be unique");
    }

    /// A failure that is not a membership refusal carries **no** reason, so the
    /// web client cannot mistake a codec or OpenMLS failure for a permission
    /// answer.
    #[test]
    fn a_non_refusal_failure_carries_no_reason() {
        let f = MlsFailure::from("internal lock poisoned");
        assert_eq!(f.reason, None);
        let f = MlsFailure::from(lakko_mls::Error::Codec("bad wire".into()));
        assert_eq!(f.reason, None);
    }

    /// End to end through the real wasm surface: the reason a client needs to
    /// pick copy arrives on the failure, not in the sentence.
    #[test]
    fn real_refusals_arrive_typed_through_the_wasm_group() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let promote = alice_g
            .set_group_roles_core(
                &alice,
                &alice.signature_public_key(),
                &bob.signature_public_key(),
            )
            .unwrap();
        bob_g
            .process_incoming_verifying_roles_core(&bob, &promote)
            .unwrap();
        charlie_g
            .process_incoming_verifying_roles_core(&charlie, &promote)
            .unwrap();

        let dave = Identity::new("dave").unwrap();
        let err = charlie_g
            .propose_add_member_core(&charlie, &dave.create_key_package().unwrap())
            .unwrap_err();
        assert_eq!(
            err.reason,
            Some(lakko_mls::RefusalReason::AddNotOwnerOrAdmin),
            "a member's add must be typed, got {err:?}",
        );

        let err = bob_g
            .set_group_roles_core(&bob, &alice.signature_public_key(), &[])
            .unwrap_err();
        assert_eq!(
            err.reason,
            Some(lakko_mls::RefusalReason::NotOwner),
            "an admin's role change must be typed, got {err:?}",
        );

        // And a wire-format failure is not a refusal at all.
        let err = alice_g
            .propose_add_member_core(&alice, b"not a key package")
            .unwrap_err();
        assert_eq!(err.reason, None, "got {err:?}");
    }

    /// The disposition crosses as data — the web client asks the core what to
    /// do instead of deciding it from a code's spelling — and the retryable set
    /// is exactly the queue-full one.
    #[test]
    fn the_refusal_disposition_is_exported_by_code() {
        assert_eq!(
            membership_refusal_disposition("roleChangePending"),
            Some("retryLater".to_string()),
        );
        assert_eq!(
            membership_refusal_disposition("addNotOwnerOrAdmin"),
            Some("notPermitted".to_string()),
        );
        // The middle value a two-way split would have got wrong: not forbidden,
        // and still not retryable.
        assert_eq!(
            membership_refusal_disposition("sweepAlreadyStaged"),
            Some("doNotRetry".to_string()),
        );
        assert_eq!(membership_refusal_disposition("not-a-code"), None);

        assert!(membership_refusal_retry_may_succeed("pendingSetFull"));
        assert!(!membership_refusal_retry_may_succeed("ownerUnremovable"));
        assert!(!membership_refusal_retry_may_succeed("sweepAlreadyStaged"));
        assert!(!membership_refusal_retry_may_succeed("not-a-code"));

        let retryable: Vec<&str> = lakko_mls::RefusalReason::ALL
            .into_iter()
            .filter(|r| membership_refusal_retry_may_succeed(r.code()))
            .map(lakko_mls::RefusalReason::code)
            .collect();
        assert_eq!(
            retryable,
            vec!["roleChangePending", "pendingSetFull"],
            "only a full bounded queue may be offered as a retry",
        );
        for reason in lakko_mls::RefusalReason::ALL {
            assert_eq!(
                membership_refusal_disposition(reason.code()),
                Some(reason.disposition().code().to_string()),
                "{reason:?}",
            );
        }
    }

    /// A second stage is refused with the STABLE machine-readable
    /// [`SWEEP_ALREADY_STAGED_CODE`] prefix, so the web client branches on a
    /// code rather than on prose (the crate's `stale:` / `invalid:` /
    /// `release:` convention).
    ///
    /// Fail-closed: building a second sweep would silently replace a commit
    /// that may already be on the wire, so the refusal is the correct
    /// behaviour and the caller's job is to reach the outstanding verdict.
    #[test]
    fn a_second_stage_is_refused_with_a_coded_error() {
        let ((alice, alice_g), _bob, _charlie) = adr0045_trio();
        let dave = Identity::new("dave").unwrap();
        let erin = Identity::new("erin").unwrap();

        alice_g
            .propose_add_member_core(&alice, &dave.create_key_package().unwrap())
            .unwrap();
        let first = alice_g
            .stage_pending_proposals_core(&alice)
            .unwrap()
            .unwrap();

        let err = alice_g.stage_pending_proposals_core(&alice).unwrap_err();
        assert!(
            err.message.starts_with(SWEEP_ALREADY_STAGED_CODE),
            "the second stage must carry the stable code, got {err:?}",
        );

        // A staged sweep also freezes this group's own membership operations,
        // so the client obligation is real: reach the verdict or stay frozen.
        assert!(
            alice_g
                .propose_add_member_core(&alice, &erin.create_key_package().unwrap())
                .is_err(),
            "a staged sweep freezes this group's own proposals",
        );

        // Reaching the verdict clears it, and the FIRST staged commit is still
        // the one that was on the wire.
        assert_eq!(
            alice_g.merge_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::Merged,
        );
        assert!(!first.commit_bytes.is_empty());
        assert!(alice_g
            .stage_pending_proposals_core(&alice)
            .unwrap()
            .is_none());
    }

    /// `hasStagedSweep` reports honestly on every path, and a verdict on a
    /// group that never staged anything is `NothingStaged` rather than an
    /// error — which is what makes both verdict calls idempotent.
    ///
    /// In particular the **eager** `commitPendingProposals` leaves nothing
    /// staged, so a rollback can never reach a commit it already merged.
    #[test]
    fn has_staged_sweep_reports_honestly_on_every_path() {
        let ((alice, alice_g), _bob, _charlie) = adr0045_trio();
        let dave = Identity::new("dave").unwrap();

        assert!(!alice_g.has_staged_sweep_core().unwrap());
        assert_eq!(
            alice_g.merge_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
        );
        assert_eq!(
            alice_g.abandon_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
        );

        // Nothing pending: staging emits no commit and stages nothing.
        assert!(alice_g
            .stage_pending_proposals_core(&alice)
            .unwrap()
            .is_none());
        assert!(
            !alice_g.has_staged_sweep_core().unwrap(),
            "an empty stage must not leave a verdict owed",
        );

        // The EAGER path merges before it returns, so it leaves nothing staged.
        alice_g
            .propose_add_member_core(&alice, &dave.create_key_package().unwrap())
            .unwrap();
        assert!(alice_g
            .commit_pending_proposals_core(&alice)
            .unwrap()
            .is_some());
        assert!(
            !alice_g.has_staged_sweep_core().unwrap(),
            "the eager sweep must leave nothing to adjudicate",
        );
        assert_eq!(
            alice_g.abandon_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::NothingStaged,
            "abandoning after an eager commit must not undo it",
        );
    }

    /// Ownership transfer round-trips through the wasm surface with **both**
    /// `OutgoingOwner` variants, and the variant is what decides the outgoing
    /// owner's fate — `Admin` keeps them in the admin set, `Member` does not.
    #[test]
    fn ownership_transfer_honours_both_outgoing_owner_variants() {
        for (outgoing, alice_keeps_admin) in
            [(OutgoingOwner::Admin, true), (OutgoingOwner::Member, false)]
        {
            let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
            let alice_key = alice.signature_public_key();
            let bob_key = bob.signature_public_key();

            let commit = alice_g
                .transfer_ownership_core(&alice, &bob_key, outgoing)
                .unwrap();
            for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
                assert_eq!(
                    g.process_incoming_verifying_roles(id, &commit)
                        .unwrap()
                        .kind(),
                    "Commit",
                    "every peer must accept the owner's transfer commit",
                );
            }

            for g in [&alice_g, &bob_g, &charlie_g] {
                assert_eq!(
                    g.role_table_owner().unwrap().expect("owner"),
                    bob_key,
                    "{outgoing:?}: bob must be the owner",
                );
                let admins_flat = g.role_table_admins().unwrap();
                let has_alice = admins_flat.chunks_exact(32).any(|a| a == alice_key);
                assert_eq!(
                    has_alice, alice_keeps_admin,
                    "{outgoing:?}: the outgoing owner's admin membership must follow the variant",
                );
            }
            assert_eq!(
                alice_g.own_committer_rank_core(&alice).unwrap().is_some(),
                alice_keeps_admin,
                "{outgoing:?}: a demoted former owner leaves the committer set entirely",
            );
            assert_eq!(bob_g.own_committer_rank_core(&bob).unwrap(), Some(0));

            // The transfer is irreversible by the person who made it.
            assert!(
                alice_g
                    .transfer_ownership_core(&alice, &alice_key, OutgoingOwner::Admin)
                    .is_err(),
                "{outgoing:?}: a transfer is irreversible by the person who made it",
            );
        }
    }

    /// The proposal-path twin: the owner **proposes** a transfer (epoch
    /// unchanged), a peer stores it, and a staged sweep installs the new owner.
    #[test]
    fn propose_ownership_transfer_rides_the_proposal_path() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let bob_key = bob.signature_public_key();
        let epoch_before = alice_g.epoch().unwrap();

        let proposal = alice_g
            .propose_ownership_transfer_core(&alice, &bob_key, OutgoingOwner::Member)
            .unwrap();
        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "a proposal must not advance the epoch",
        );

        for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
            let record = g
                .process_incoming_verifying_roles(id, &proposal)
                .unwrap()
                .proposal()
                .expect("a Proposal carries a record");
            assert!(record.stored(), "the owner's role change must be stored");
            assert_eq!(record.kind(), "role-change");
        }

        let staged = alice_g
            .stage_pending_proposals_core(&alice)
            .unwrap()
            .unwrap();
        for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
            assert_eq!(
                g.process_incoming_verifying_roles(id, &staged.commit_bytes)
                    .unwrap()
                    .kind(),
                "Commit",
            );
        }
        assert_eq!(
            alice_g.merge_staged_sweep_core(&alice).unwrap(),
            StagedSweepOutcome::Merged,
        );

        for g in [&alice_g, &bob_g, &charlie_g] {
            assert_eq!(g.role_table_owner().unwrap().expect("owner"), bob_key);
        }
        assert_eq!(bob_g.own_committer_rank_core(&bob).unwrap(), Some(0));
        assert_eq!(alice_g.own_committer_rank_core(&alice).unwrap(), None);
    }

    /// C-63 at the FFI: naming an owner that no leaf holds is refused locally
    /// with a clear error, before any MLS state is touched. An owner outside
    /// the group could never be replaced (a role change is owner-only) nor
    /// removed (the owner's leaf is unremovable), so the group would be
    /// permanently un-administrable.
    #[test]
    fn transfer_refuses_a_non_member_and_a_bad_key_length() {
        let ((alice, alice_g), _bob, _charlie) = adr0045_trio();
        let stranger = Identity::new("stranger").unwrap();
        let stranger_key = stranger.signature_public_key();
        let epoch_before = alice_g.epoch().unwrap();

        for outgoing in [OutgoingOwner::Admin, OutgoingOwner::Member] {
            assert!(
                alice_g
                    .transfer_ownership_core(&alice, &stranger_key, outgoing)
                    .is_err(),
                "{outgoing:?}: an owner outside the group must be refused",
            );
            assert!(alice_g
                .propose_ownership_transfer_core(&alice, &stranger_key, outgoing)
                .is_err());
            assert!(alice_g
                .transfer_ownership_core(&alice, &[0u8; 31], outgoing)
                .is_err());
            assert!(alice_g
                .propose_ownership_transfer_core(&alice, &[0u8; 33], outgoing)
                .is_err());
        }

        assert_eq!(
            alice_g.epoch().unwrap(),
            epoch_before,
            "every refusal must leave the group exactly as it was",
        );
        assert!(!alice_g.has_staged_sweep_core().unwrap());
        assert!(alice_g.pending_proposals_core().unwrap().is_empty());
    }

    /// A plain member may neither stage a sweep nor transfer ownership.
    #[test]
    fn a_plain_member_can_neither_stage_nor_transfer() {
        let ((alice, alice_g), _bob, (charlie, charlie_g)) = adr0045_trio();
        let charlie_key = charlie.signature_public_key();

        assert!(
            charlie_g.stage_pending_proposals_core(&charlie).is_err(),
            "a plain member may not sweep, staged or eager",
        );
        assert!(charlie_g
            .transfer_ownership_core(&charlie, &charlie_key, OutgoingOwner::Admin)
            .is_err());
        assert!(charlie_g
            .propose_ownership_transfer_core(&charlie, &charlie_key, OutgoingOwner::Member)
            .is_err());
        assert!(!charlie_g.has_staged_sweep_core().unwrap());
        assert_eq!(
            alice_g.role_table_owner().unwrap().expect("owner"),
            alice.signature_public_key(),
        );
    }

    /// **No `OutgoingOwner` variant may be the integer `0`.**
    ///
    /// A missing or `undefined` JS argument reaches the wasm ABI as `0`. If a
    /// variant occupied `0`, omitting the argument would silently select it —
    /// exactly the "collapse to a default" the core refuses to allow. With the
    /// discriminants starting at 1, `0` matches no variant and wasm-bindgen's
    /// generated `FromWasmAbi` throws `"invalid enum value passed"` instead.
    ///
    /// The same reasoning covers `StagedSweepOutcome` on the return side: a
    /// `0` variant would be JavaScript-falsy, so `if (outcome)` would silently
    /// read as "no outcome".
    #[test]
    fn outgoing_owner_variants_are_never_zero() {
        assert_eq!(OutgoingOwner::Admin as u32, 1);
        assert_eq!(OutgoingOwner::Member as u32, 2);
        assert_eq!(StagedSweepOutcome::Merged as u32, 1);
        assert_eq!(StagedSweepOutcome::Abandoned as u32, 2);
        assert_eq!(StagedSweepOutcome::NothingStaged as u32, 3);
    }

    #[test]
    fn identity_signature_public_key_is_32_bytes() {
        let id = Identity::new("alice").unwrap();
        let pk = id.signature_public_key();
        assert_eq!(pk.len(), 32, "Ed25519 public key length");
    }

    /// Phase 1.16e6b — round-trip a live group through
    /// `Identity.snapshotStorage` / `Identity.restoreStorage` +
    /// `Group.load` and confirm the post-restore group reports
    /// the same `groupId`, `memberCount`, and own leaf index.
    /// Mirrors the host-only Rust integration test in
    /// `crates/lakko-mls/tests/end_to_end.rs` — JS-side error
    /// paths (panics in `JsError::new`) stay in
    /// `wasm-bindgen-test` per the rest of the FFI suite.
    #[test]
    fn snapshot_restore_load_round_trips_a_group() {
        let seed = [0xE6u8; 64];
        let alice = Identity::from_seed(&seed).expect("alice");
        let group = Group::new(&alice).expect("group");
        let group_id = group.group_id().expect("group_id");
        let initial_members = group.member_count().expect("count");
        let initial_leaf = group.own_leaf_index().expect("leaf");

        let snapshot = alice.snapshot_storage().expect("snapshot");
        assert!(!snapshot.is_empty());

        // Drop the original alice + group entirely; re-derive
        // alice from the same seed and restore.
        drop(group);
        drop(alice);

        let restored_alice = Identity::from_seed(&seed).expect("restored alice");
        restored_alice.restore_storage(&snapshot).expect("restore");

        let restored_group = Group::load(&restored_alice, &group_id)
            .expect("load")
            .expect("group present");
        assert_eq!(restored_group.group_id().expect("gid"), group_id);
        assert_eq!(restored_group.member_count().expect("mc"), initial_members);
        assert_eq!(restored_group.own_leaf_index().expect("li"), initial_leaf);
    }

    /// `Group.load` returns `None` when no group is persisted
    /// under that id — the JS side reads `undefined`.
    #[test]
    fn group_load_returns_none_for_unknown_id() {
        let alice = Identity::from_seed(&[0xE7u8; 64]).expect("alice");
        let arbitrary = [0x99u8; 32];
        let loaded = Group::load(&alice, &arbitrary).expect("load");
        assert!(loaded.is_none());
    }

    /// Phase 1.16d3b2 — `Identity.fromSeed` is deterministic.
    /// Same seed bytes ⇒ same Ed25519 + X25519 public keys.
    #[test]
    fn identity_from_seed_is_deterministic() {
        let seed = [0x77u8; 64];
        let a = Identity::from_seed(&seed).unwrap();
        let b = Identity::from_seed(&seed).unwrap();
        assert_eq!(a.signature_public_key(), b.signature_public_key());
        assert_eq!(a.x25519_public_key(), b.x25519_public_key());
    }

    /// Phase 1.16d3b — the X25519 envelope keypair surfaces as a
    /// pair of 32-byte `Vec<u8>` getters. Pubkey copies are fresh
    /// each call (idempotent, no shared mutable state).
    #[test]
    fn identity_x25519_keys_are_32_bytes_each() {
        let id = Identity::new("alice").unwrap();
        let pk = id.x25519_public_key();
        let sk = id.x25519_secret_key();
        assert_eq!(pk.len(), 32, "X25519 public key length");
        assert_eq!(sk.len(), 32, "X25519 secret key length");
        // Sanity: at least one byte non-zero on each — would
        // catch a default-initialised secret slipping through.
        assert!(pk.iter().any(|&b| b != 0));
        assert!(sk.iter().any(|&b| b != 0));
        // Idempotent reads — getters return copies of the same
        // underlying secret.
        assert_eq!(id.x25519_public_key(), pk);
        assert_eq!(id.x25519_secret_key(), sk);
    }

    #[test]
    fn processed_message_kind_strings_match_uniffi_enum() {
        // Every variant has a string literal — guards against
        // accidental rename on the Rust side breaking the JS API.
        let app = ProcessedMessage::application(vec![1, 2, 3], Some(0), None);
        assert_eq!(app.kind(), "Application");
        assert_eq!(app.application_data().as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(app.sender_leaf(), Some(0));

        for k in [
            ProcessedMessageKind::Commit,
            ProcessedMessageKind::Proposal,
            ProcessedMessageKind::ExternalJoinProposal,
        ] {
            let m = ProcessedMessage::control(k);
            // Each non-Application case has no data.
            assert!(m.application_data().is_none(), "kind={k:?}");
            // And a non-empty discriminator string.
            assert!(!m.kind().is_empty());
        }
    }

    #[test]
    fn add_member_record_clones_on_each_getter_call() {
        // Confirm the getters don't move bytes out — JS callers can
        // read .commit / .welcome multiple times.
        let r = AddMember {
            commit_bytes: vec![0xAA; 4],
            welcome_bytes: vec![0xBB; 4],
        };
        assert_eq!(r.commit(), vec![0xAA; 4]);
        assert_eq!(r.commit(), vec![0xAA; 4]); // call twice → still same
        assert_eq!(r.welcome(), vec![0xBB; 4]);
    }

    // ============================================================================
    // PublisherGroup wasm surface (G4-2-mls-persistence step 2)
    // ============================================================================

    fn channel_id_filled(byte: u8) -> Vec<u8> {
        vec![byte; lakko_mls::CHANNEL_ID_LEN]
    }

    #[test]
    fn wasm_publisher_group_create_then_seed_is_stable() {
        let alice = Identity::new("alice-pub").unwrap();
        let cid = channel_id_filled(0xAA);

        let pg = PublisherGroup::new(&alice, &cid).unwrap();
        let s1 = pg.channel_seed(&alice).unwrap();
        let s2 = pg.channel_seed(&alice).unwrap();
        assert_eq!(s1, s2);
        assert_eq!(s1.len(), 32);
        assert_eq!(pg.channel_id().unwrap(), cid);
    }

    #[test]
    fn wasm_publisher_group_open_or_create_is_idempotent() {
        // Same load-bearing test as the UniFFI mirror —
        // proves the throwaway-group bug stays fixed under
        // the web composer's call shape.
        let alice = Identity::new("alice-persist").unwrap();
        let cid = channel_id_filled(0xBB);

        let first = PublisherGroup::open_or_create(&alice, &cid).unwrap();
        let s1 = first.channel_seed(&alice).unwrap();

        let second = PublisherGroup::open_or_create(&alice, &cid).unwrap();
        let s2 = second.channel_seed(&alice).unwrap();

        assert_eq!(s1, s2);
    }

    #[test]
    fn wasm_publisher_group_load_returns_none_before_create() {
        let alice = Identity::new("alice-empty").unwrap();
        let cid = channel_id_filled(0xCC);
        let loaded = PublisherGroup::load(&alice, &cid).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn wasm_publisher_group_rotate_changes_seed() {
        // G4-2-mls-persistence step 4 (wasm). Mirror of the
        // UniFFI rotate test.
        let alice = Identity::new("alice-rotate").unwrap();
        let cid = channel_id_filled(0xEE);

        let pg = PublisherGroup::open_or_create(&alice, &cid).unwrap();
        let seed_pre = pg.channel_seed(&alice).unwrap();

        let _commit = pg.rotate(&alice).unwrap();
        let seed_post = pg.channel_seed(&alice).unwrap();

        assert_ne!(seed_pre, seed_post);
        assert_eq!(seed_post.len(), 32);
    }

    /// The peer-facing rank — the input `BeaconObservation.rank` needs. It must
    /// agree with [`Group::own_committer_rank`] from every vantage point (the
    /// rank is a function of the shared, authenticated role table, so it is
    /// never observer-relative), and must fail closed to `undefined` for a
    /// non-member, a wrong-length key, and an open group.
    #[test]
    fn peer_committer_rank_agrees_with_own_rank_from_every_vantage_point() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let owner_key = alice.signature_public_key();
        let bob_key = bob.signature_public_key();
        let charlie_key = charlie.signature_public_key();

        let promote = alice_g
            .set_group_roles(&alice, &owner_key, &bob_key)
            .unwrap();
        for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
            assert_eq!(
                g.process_incoming_verifying_roles(id, &promote)
                    .unwrap()
                    .kind(),
                "Commit",
            );
        }

        for (label, g) in [
            ("alice", &alice_g),
            ("bob", &bob_g),
            ("charlie", &charlie_g),
        ] {
            assert_eq!(
                g.committer_rank_core(&owner_key).unwrap(),
                Some(0),
                "{label} must rank the owner 0",
            );
            assert_eq!(
                g.committer_rank_core(&bob_key).unwrap(),
                Some(1),
                "{label} must rank the sole admin 1",
            );
            assert_eq!(
                g.committer_rank_core(&charlie_key).unwrap(),
                None,
                "{label} must give a plain member NO rank — not rank 0",
            );
        }

        for (id, g) in [(&alice, &alice_g), (&bob, &bob_g), (&charlie, &charlie_g)] {
            assert_eq!(
                g.committer_rank_core(&id.signature_public_key()).unwrap(),
                g.own_committer_rank_core(id).unwrap(),
            );
        }

        let dave = Identity::new("dave").unwrap();
        assert_eq!(
            alice_g
                .committer_rank_core(&dave.signature_public_key())
                .unwrap(),
            None,
            "a non-member has no rank",
        );
        assert_eq!(alice_g.committer_rank_core(&[0u8; 31]).unwrap(), None);
        assert_eq!(alice_g.committer_rank_core(&[]).unwrap(), None);
    }

    /// A rank is a **position** in the role table's canonical order, not a
    /// stable identity — which is why `BeaconObservation.rank` must be derived
    /// when the verdict is taken and never captured at receipt.
    ///
    /// Promoting a second admin whose key sorts BEFORE the incumbent moves the
    /// incumbent from rank 1 to rank 2, so a rank captured under the old table
    /// names a different position in the new one — incommensurable, not merely
    /// stale. Both sort directions are exercised, so the test cannot pass by
    /// accident on whichever key happens to sort lower.
    #[test]
    fn a_rank_is_a_position_that_an_admin_change_renumbers() {
        let ((alice, alice_g), (bob, bob_g), (charlie, charlie_g)) = adr0045_trio();
        let owner_key = alice.signature_public_key();
        let bob_key = bob.signature_public_key();
        let charlie_key = charlie.signature_public_key();

        let apply = |wire: &[u8]| {
            for (id, g) in [(&bob, &bob_g), (&charlie, &charlie_g)] {
                assert_eq!(
                    g.process_incoming_verifying_roles(id, wire).unwrap().kind(),
                    "Commit",
                );
            }
        };

        apply(
            &alice_g
                .set_group_roles(&alice, &owner_key, &bob_key)
                .unwrap(),
        );
        assert_eq!(alice_g.committer_rank_core(&bob_key).unwrap(), Some(1));

        let mut both = bob_key.clone();
        both.extend_from_slice(&charlie_key);
        apply(&alice_g.set_group_roles(&alice, &owner_key, &both).unwrap());

        let bob_now = alice_g.committer_rank_core(&bob_key).unwrap();
        let charlie_now = alice_g.committer_rank_core(&charlie_key).unwrap();
        assert_ne!(bob_now, charlie_now, "two admins cannot share a rank");
        let mut ranks = [bob_now, charlie_now];
        ranks.sort_unstable();
        assert_eq!(ranks, [Some(1), Some(2)]);
        if bob_key < charlie_key {
            assert_eq!(bob_now, Some(1));
            assert_eq!(charlie_now, Some(2));
        } else {
            assert_eq!(
                bob_now,
                Some(2),
                "adding an admin that sorts first renumbers the incumbent from 1 to 2",
            );
            assert_eq!(charlie_now, Some(1));
        }
    }

    // `wasm_publisher_group_rejects_short_channel_id` lives in
    // the UniFFI mirror (`crates/lakko-ffi-uniffi`) only. The
    // wasm-bindgen `JsError` constructor panics under cargo
    // test (off-wasm target), so error-path tests that
    // produce a `JsError` cannot execute here. The Rust-side
    // channel-id length check it would exercise is the same
    // `MlsChannelId::try_from_slice` predicate the UniFFI
    // tests already pin.
}

/// Wrap an added member's Welcome into a rendezvous envelope the sweeping
/// device posts to the group journal (ADR-0027 Phase 3, G-3).
/// `joinerSignatureKey` is the added member's signature key; `welcome` is the
/// `StagedSweep.welcomeBytes` the ADR-0045 sweep produced (or, on a
/// `PublisherGroup`, the `AddMember.welcomeBytes` from its direct
/// `approveExternalJoin`).
///
/// **A sweep cannot tell which of its adds came from a `/g/` link** — that is
/// what makes an approval indistinguishable from an ordinary add, and it is
/// deliberate — so the caller wraps one envelope per added key, origin-blind.
#[wasm_bindgen(js_name = wrapWelcomeRendezvous)]
pub fn wrap_welcome_rendezvous(
    joiner_signature_key: &[u8],
    welcome: &[u8],
) -> Result<Vec<u8>, JsValue> {
    lakko_mls::WelcomeRendezvous {
        joiner_signature_key: joiner_signature_key.to_vec(),
        welcome: welcome.to_vec(),
    }
    .encode()
    .map_err(mls_err)
}

/// Decode a group-journal entry as a rendezvous envelope and, if it is
/// addressed to `mySignatureKey`, return the inner Welcome (ADR-0027 Phase 3).
/// Returns `undefined` when the entry is not a rendezvous, is malformed, or is
/// addressed to someone else — so the joiner can run it over **every** journal
/// entry without throwing on the non-matching ones.
#[wasm_bindgen(js_name = welcomeRendezvousFor)]
pub fn welcome_rendezvous_for(bytes: &[u8], my_signature_key: &[u8]) -> Option<Vec<u8>> {
    match lakko_mls::WelcomeRendezvous::decode(bytes) {
        Ok(r) if r.is_for(my_signature_key) => Some(r.welcome),
        _ => None,
    }
}

/// Read a group's current MLS epoch from a wire-form journal message's public
/// framing, without any group secret (ADR-0028 §3 — the join-ticket joiner
/// side). Returns `undefined` for non-MLS bytes or an epoch-less variant
/// (Welcome / KeyPackage) — the caller treats that as epoch `0`.
#[wasm_bindgen(js_name = epochOfMessage)]
#[must_use]
pub fn epoch_of_message(bytes: &[u8]) -> Option<u64> {
    lakko_mls::epoch_of_message(bytes)
}

/// The epoch the group is in AFTER a journal row (ADR-0028 §3): the framed
/// epoch, plus one when the row is a Commit. Over the journal's last MLS row
/// this is the number a joiner puts on its external-join proposal; the framed
/// epoch is stale whenever the head is a Commit and the proposal is then
/// silently refused. Returns `undefined` exactly where `epochOfMessage` does.
#[wasm_bindgen(js_name = epochAfterMessage)]
#[must_use]
pub fn epoch_after_message(bytes: &[u8]) -> Option<u64> {
    lakko_mls::epoch_after_message(bytes)
}
