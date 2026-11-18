// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of `lakko-vault` - the conversation vault
//! ([ADR-0064]) and the blob delete key ([ADR-0065] section 6).
//!
//! Mirror of `lakko-ffi-uniffi/src/vault.rs`; the function names are the
//! same. A vault is an object on the storage service, one per conversation
//! plus two per account (the index and the devices vault), that no request
//! ties to the account. This module hands the web client:
//!
//! - [`VaultKeys`], built from the ACCOUNT identity by `vaultKeys(identity)`:
//!   a vault's name, the public half of its write key, sealing and opening
//!   of its objects, and the request signature of the storage routes. It
//!   never returns the vault root, a vault key or a write seed.
//! - [`VaultRef`], which vault: `VaultRef.conversation(refKind, refId)` (the
//!   ADR-0058 reference, `refKind` `1` one-to-one, `2` group, `3` channel),
//!   `VaultRef.index()` or `VaultRef.devices()`.
//! - The saved records, as classes that are both built and read:
//!   [`VaultConversationHead`], [`VaultMessage`], [`VaultAttachment`] and
//!   the small row classes, with `encodeConversationHead` /
//!   `decodeConversationHead`, `encodeHistoryChunk` / `decodeHistoryChunk`,
//!   `encodeVaultIndex` / `decodeVaultIndex`, `encodeDevicesHead` /
//!   `decodeDevicesHead` and `mergeVaultMessages`.
//!
//! ## Shapes
//!
//! Follows the history-placeholder classes of `mls.rs`: a constructor takes
//! the required fields, setters add the optional ones, flat getters read
//! them back. Unlike those, one class serves both directions, because
//! `mergeVaultMessages` returns what it takes. Enums cross as their wire
//! number (`direction` `0` outgoing / `1` incoming / `2` system; conversation
//! `kind` `0` unknown / `1` one-to-one / `2` group; `role` `0` member / `1`
//! admin / `2` owner; `membership` `0` in / `1` left / `2` removed). 64-bit
//! integers are `bigint`.
//!
//! **An array argument is consumed.** wasm-bindgen moves the objects of an
//! `Array<VaultMessage>` (or any other array of these classes) into the
//! call: afterwards the JS handles are empty and must not be used. Read the
//! getters of the result instead.
//!
//! ## Media metadata
//!
//! The core codec's bounds are structural. The ADR-0047 media rules live in
//! `lakko-attachments`, and this module applies
//! `lakko_attachments::RawMediaMetadata::bounded` wherever an attachment
//! enters a [`VaultAttachment`]: in `setMedia`, in `setThumb`, and on every
//! decode. So no instance carries metadata an ordinary received message
//! could not - a field outside its bound is dropped alone, a poster frame
//! outside its size limit is dropped whole - and that holds for whatever is
//! encoded or merged from them.
//!
//! ## Errors
//!
//! Every refusal throws an `Error` whose message is `"<code>: <reason>"`.
//! The code is the core's frozen `code()` (`RecordError`, `SealError`,
//! `ProofError`, `VaultRefError`), or one of the argument-length codes this
//! module adds: `blobIdLength`, `keyLength`, `nonceLength`,
//! `accountKeyLength`, `seedLength`. An enum number outside its range is
//! `unknownOption` (`unknownKind` for a `refKind`). Branch on the code,
//! never on the prose.
//!
//! The clock is `Date.now()` on wasm32 and the system clock on the host,
//! where the unit tests run; the nonce is `getrandom` under its `js` feature
//! either way.
//!
//! [ADR-0064]: ../../../docs/adr/0064-conversation-backup-and-device-re-entry.md
//! [ADR-0065]: ../../../docs/adr/0065-attachment-lifetime-and-access.md

use std::fmt;

use lakko_attachments::{RawMediaMetadata, RawThumbMetadata};
use lakko_crypto::aead::NONCE_LEN;
use lakko_crypto::SecretBytes;
use lakko_vault::keys::{VaultRef as CoreVaultRef, VaultRefError, VaultRoot};
use lakko_vault::proof::{self, ProofError, PROOF_HEADER_NAME, PROOF_WINDOW_MS};
use lakko_vault::record::{
    self as rec, RecordError, MAX_CHUNK_MESSAGES, MAX_GROUP_INFO_BYTES, MAX_PEOPLE,
    MAX_REENTRY_EPOCHS, MAX_TAIL_MESSAGES,
};
use lakko_vault::seal::{open_object, seal_object, sealed_len, ObjectKind, SealError};
use lakko_vault::{merge_messages, ConversationRef as CoreConversationRef};
use wasm_bindgen::prelude::*;

use crate::mls::Identity;

// --- errors -------------------------------------------------------------

/// A refusal on its way to a thrown `Error`: `"<code>: <reason>"`.
///
/// Its own type, and a `std::error::Error`, so an exported function converts
/// it with `?` (wasm-bindgen's blanket `From<E: Error> for JsError`) while
/// the host tests, which cannot build a `JsError`, read the message from the
/// `try_*` function underneath.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal(String);

impl Refusal {
    fn new(code: &str, reason: impl fmt::Display) -> Self {
        Self(format!("{code}: {reason}"))
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

impl From<RecordError> for Refusal {
    fn from(e: RecordError) -> Self {
        Self::new(e.code(), e)
    }
}

impl From<SealError> for Refusal {
    fn from(e: SealError) -> Self {
        Self::new(e.code(), e)
    }
}

impl From<ProofError> for Refusal {
    fn from(e: ProofError) -> Self {
        Self::new(e.code(), e)
    }
}

impl From<VaultRefError> for Refusal {
    fn from(e: VaultRefError) -> Self {
        Self::new(e.code(), e)
    }
}

fn fixed<const N: usize>(bytes: &[u8], code: &str, what: &str) -> Result<[u8; N], Refusal> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        Refusal::new(
            code,
            format_args!("{what} must be {N} bytes, got {}", bytes.len()),
        )
    })
}

/// Read an enum's wire number; anything outside its range is refused with
/// the record codec's own code for that.
fn option<T>(byte: u8, read: fn(u8) -> Option<T>, what: &str) -> Result<T, Refusal> {
    read(byte).ok_or_else(|| {
        Refusal::new(
            RecordError::UnknownOption.code(),
            format_args!("{what} {byte} is not a known value"),
        )
    })
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    // `Date.now()` is integral milliseconds carried in an f64; a negative or
    // non-finite value (a broken clock) reads as 0 and the server refuses.
    let now = js_sys::Date::now();
    if now.is_finite() && now >= 0.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = now as u64;
        ms
    } else {
        0
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn as_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn some_text(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

fn narrow(px: Option<u32>) -> Option<u16> {
    px.and_then(|px| u16::try_from(px).ok())
}

fn conversation(ref_kind: u8, ref_id: &[u8]) -> Result<CoreConversationRef, Refusal> {
    Ok(CoreConversationRef::from_wire(ref_kind, ref_id)?)
}

// --- which vault --------------------------------------------------------

/// Which vault: one conversation's, or one of the two account-level vaults.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRef {
    inner: CoreVaultRef,
}

impl VaultRef {
    fn try_conversation(ref_kind: u8, ref_id: &[u8]) -> Result<Self, Refusal> {
        Ok(Self {
            inner: CoreVaultRef::Conversation(conversation(ref_kind, ref_id)?),
        })
    }
}

#[wasm_bindgen]
impl VaultRef {
    /// The vault of one conversation or channel, named the ADR-0058 way:
    /// `refKind` is `1` one-to-one, `2` group, `3` channel; `refId` the MLS
    /// group id or the channel id (1..=64 bytes).
    ///
    /// # Errors
    ///
    /// Throws `unknownKind: ...` or `idLength: ...`.
    pub fn conversation(ref_kind: u8, ref_id: &[u8]) -> Result<VaultRef, JsError> {
        Ok(Self::try_conversation(ref_kind, ref_id)?)
    }

    /// The vault that lists the account's conversations.
    pub fn index() -> VaultRef {
        Self {
            inner: CoreVaultRef::Index,
        }
    }

    /// The vault of the account's own device group.
    pub fn devices() -> VaultRef {
        Self {
            inner: CoreVaultRef::Devices,
        }
    }
}

// --- keys ---------------------------------------------------------------

/// The vault names and keys of one account. See the module doc.
///
/// Holds the vault root, from which every vault of the account can be named,
/// read and rewritten and every file the account uploaded deleted. The root
/// and what it derives stay inside this object: no method returns the root, a
/// vault key or a write seed.
#[wasm_bindgen]
pub struct VaultKeys {
    root: VaultRoot,
}

/// The vault keys of `accountIdentity`'s account.
///
/// `accountIdentity` is the ACCOUNT identity (the ADR-0017 phrase-seeded
/// one, the same on every device of the account), not a device identity:
/// its Ed25519 seed is the derivation input, exactly as for
/// `dsJournalRootSigner`. A device identity here would name vaults no other
/// device of the account can find.
#[wasm_bindgen(js_name = vaultKeys)]
pub fn vault_keys(account_identity: &Identity) -> VaultKeys {
    let seed = SecretBytes::new(account_identity.mls_identity().ed25519_seed_bytes());
    VaultKeys {
        root: VaultRoot::from_account_seed(seed.as_bytes()),
    }
}

impl VaultKeys {
    fn try_seal(
        &self,
        vault: &VaultRef,
        kind: ObjectKind,
        chunk_no: u32,
        version: u64,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, Refusal> {
        let mut nonce = [0u8; NONCE_LEN];
        lakko_crypto::random::fill(&mut nonce);
        Ok(seal_object(
            &self.root.vault_key(&vault.inner),
            &self.root.vault_id(&vault.inner),
            kind,
            chunk_no,
            version,
            &nonce,
            plaintext,
        )?)
    }

    fn try_open(
        &self,
        vault: &VaultRef,
        kind: ObjectKind,
        chunk_no: u32,
        version: u64,
        sealed: &[u8],
    ) -> Result<Vec<u8>, Refusal> {
        Ok(open_object(
            &self.root.vault_key(&vault.inner),
            &self.root.vault_id(&vault.inner),
            kind,
            chunk_no,
            version,
            sealed,
        )?)
    }

    fn try_sign_request(
        &self,
        vault: &VaultRef,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<String, Refusal> {
        let seed = self.root.vault_write_seed(&vault.inner);
        Ok(proof::sign(seed.as_bytes(), method, path, body, now_ms())?)
    }

    fn try_blob_delete_public_key(&self, blob_id: &[u8]) -> Result<Vec<u8>, Refusal> {
        let blob_id = fixed::<32>(blob_id, "blobIdLength", "blob id")?;
        let seed = self.root.blob_delete_seed(&blob_id);
        Ok(proof::public_key(seed.as_bytes()).to_vec())
    }

    fn try_sign_blob_delete(
        &self,
        blob_id: &[u8],
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<String, Refusal> {
        let blob_id = fixed::<32>(blob_id, "blobIdLength", "blob id")?;
        let seed = self.root.blob_delete_seed(&blob_id);
        Ok(proof::sign(seed.as_bytes(), method, path, body, now_ms())?)
    }
}

#[wasm_bindgen]
impl VaultKeys {
    /// The 32-byte name of the vault on the storage service: what the request
    /// path carries, in lowercase hex. Not secret, and never sent to another
    /// person.
    #[wasm_bindgen(js_name = vaultId)]
    pub fn vault_id(&self, vault: &VaultRef) -> Vec<u8> {
        self.root.vault_id(&vault.inner).to_vec()
    }

    /// The 32-byte Ed25519 public key a claim (`PUT /vault/{id}`) stores: the
    /// public half of the vault's write key.
    #[wasm_bindgen(js_name = writePublicKey)]
    pub fn write_public_key(&self, vault: &VaultRef) -> Vec<u8> {
        let seed = self.root.vault_write_seed(&vault.inner);
        proof::public_key(seed.as_bytes()).to_vec()
    }

    /// Seal `plaintext` (an encoded head) as the vault's head at `version`.
    /// The 12-byte nonce is drawn here from the platform's random source,
    /// fresh for every call; the result is padded to a size bucket.
    ///
    /// # Errors
    ///
    /// Throws `plaintextTooLarge: ...` or `seal: ...`.
    #[wasm_bindgen(js_name = sealHead)]
    pub fn seal_head(
        &self,
        vault: &VaultRef,
        version: u64,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        Ok(self.try_seal(vault, ObjectKind::Head, 0, version, plaintext)?)
    }

    /// Seal `plaintext` (an encoded chunk) as chunk `chunkNo` of the vault at
    /// `version`. The nonce is drawn here, as for `sealHead`.
    ///
    /// # Errors
    ///
    /// Throws `plaintextTooLarge: ...` or `seal: ...`.
    #[wasm_bindgen(js_name = sealChunk)]
    pub fn seal_chunk(
        &self,
        vault: &VaultRef,
        chunk_no: u32,
        version: u64,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        Ok(self.try_seal(vault, ObjectKind::Chunk, chunk_no, version, plaintext)?)
    }

    /// Open bytes the storage service returned as the vault's head at
    /// `version`. Bytes sealed as anything else - another vault, a chunk,
    /// another version - fail with `authentication`.
    ///
    /// # Errors
    ///
    /// Throws `objectTooLarge`, `truncated`, `unknownFormat`, `notABucket`,
    /// `authentication` or `padding`, each followed by `": "` and the reason.
    #[wasm_bindgen(js_name = openHead)]
    pub fn open_head(
        &self,
        vault: &VaultRef,
        version: u64,
        sealed: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        Ok(self.try_open(vault, ObjectKind::Head, 0, version, sealed)?)
    }

    /// Open bytes the storage service returned as chunk `chunkNo` of the
    /// vault at `version`. Bytes sealed as anything else - another vault, a
    /// head, another chunk number, another version - fail with
    /// `authentication`.
    ///
    /// # Errors
    ///
    /// As `openHead`.
    #[wasm_bindgen(js_name = openChunk)]
    pub fn open_chunk(
        &self,
        vault: &VaultRef,
        chunk_no: u32,
        version: u64,
        sealed: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        Ok(self.try_open(vault, ObjectKind::Chunk, chunk_no, version, sealed)?)
    }

    /// The `storageProofHeaderName()` header value for one request to the
    /// vault's routes, signed with the vault's write key. `method` is upper
    /// case (`GET`, `PUT`, `DELETE`); `path` is the service path
    /// (`/vault/<hex>/head`), never a reverse-proxy prefix; `body` is the
    /// exact bytes sent (empty for a `GET`). The clock is read here; a value
    /// is good within `storageProofWindowMs()` of the server's clock.
    ///
    /// # Errors
    ///
    /// Throws `requestNotCanonical: ...` when `method` or `path` contains a
    /// NUL byte.
    #[wasm_bindgen(js_name = signRequest)]
    pub fn sign_request(
        &self,
        vault: &VaultRef,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<String, JsError> {
        Ok(self.try_sign_request(vault, method, path, body)?)
    }

    /// The 32-byte Ed25519 public key an upload of the blob `blobId`
    /// carries, so that only a device of this account can delete it
    /// (ADR-0065 section 6).
    ///
    /// # Errors
    ///
    /// Throws `blobIdLength: ...` when `blobId` is not 32 bytes.
    #[wasm_bindgen(js_name = blobDeletePublicKey)]
    pub fn blob_delete_public_key(&self, blob_id: &[u8]) -> Result<Vec<u8>, JsError> {
        Ok(self.try_blob_delete_public_key(blob_id)?)
    }

    /// The `storageProofHeaderName()` header value for `DELETE /blob/{hex}`
    /// of the blob `blobId`, signed with that blob's delete key. Arguments as
    /// for `signRequest`.
    ///
    /// # Errors
    ///
    /// Throws `blobIdLength: ...` or `requestNotCanonical: ...`.
    #[wasm_bindgen(js_name = signBlobDelete)]
    pub fn sign_blob_delete(
        &self,
        blob_id: &[u8],
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<String, JsError> {
        Ok(self.try_sign_blob_delete(blob_id, method, path, body)?)
    }
}

/// The request header a storage proof travels in (`x-lakko-storage-proof`).
#[wasm_bindgen(js_name = storageProofHeaderName)]
pub fn storage_proof_header_name() -> String {
    PROOF_HEADER_NAME.to_string()
}

/// How far from the server's clock a storage proof's time may lie, either
/// way, in milliseconds (120 000). A `bigint`.
#[wasm_bindgen(js_name = storageProofWindowMs)]
pub fn storage_proof_window_ms() -> u64 {
    PROOF_WINDOW_MS
}

/// The sealed size of an object carrying `plaintextLen` bytes - what the
/// storage service will see - or `undefined` when that is more than one
/// object can carry. For a client that budgets a vault's bytes. `bigint` in
/// and out.
#[wasm_bindgen(js_name = vaultSealedLen)]
pub fn vault_sealed_len(plaintext_len: u64) -> Option<u64> {
    let len = usize::try_from(plaintext_len).ok()?;
    u64::try_from(sealed_len(len)?).ok()
}

/// Most messages in one chunk (512).
#[wasm_bindgen(js_name = vaultMaxChunkMessages)]
pub fn vault_max_chunk_messages() -> u32 {
    as_u32(MAX_CHUNK_MESSAGES)
}

/// Most messages in a head's tail (512).
#[wasm_bindgen(js_name = vaultMaxTailMessages)]
pub fn vault_max_tail_messages() -> u32 {
    as_u32(MAX_TAIL_MESSAGES)
}

/// Most saved epochs of re-entry material in a head (2: the two newest).
#[wasm_bindgen(js_name = vaultMaxReentryEpochs)]
pub fn vault_max_reentry_epochs() -> u32 {
    as_u32(MAX_REENTRY_EPOCHS)
}

/// Most people in one head (4096).
#[wasm_bindgen(js_name = vaultMaxPeople)]
pub fn vault_max_people() -> u32 {
    as_u32(MAX_PEOPLE)
}

/// Longest saved GroupInfo, in bytes (1 MiB).
#[wasm_bindgen(js_name = vaultMaxGroupInfoBytes)]
pub fn vault_max_group_info_bytes() -> u32 {
    as_u32(MAX_GROUP_INFO_BYTES)
}

// --- attachments --------------------------------------------------------

/// Apply the ADR-0047 bounds (`lakko_attachments::RawMediaMetadata`) to a
/// saved attachment, so a restored row keeps exactly what a live envelope
/// would have kept.
fn bound_media(a: &mut rec::VaultAttachment) {
    let media = RawMediaMetadata {
        kind: some_text(&a.kind),
        duration_ms: a.duration_ms,
        waveform: Some(std::mem::take(&mut a.waveform)).filter(|w| !w.is_empty()),
        width: a.width.map(u32::from),
        height: a.height.map(u32::from),
        thumb: a.thumb.as_ref().map(|t| RawThumbMetadata {
            size: u64::from(t.size),
            mime: some_text(&t.mime),
            width: t.width.map(u32::from),
            height: t.height.map(u32::from),
        }),
        title: some_text(&a.title),
        artist: some_text(&a.artist),
    }
    .bounded();
    a.title = media.title.unwrap_or_default();
    a.artist = media.artist.unwrap_or_default();
    a.kind = media.kind.unwrap_or_default();
    a.duration_ms = media.duration_ms;
    a.waveform = media.waveform.unwrap_or_default();
    a.width = narrow(media.width);
    a.height = narrow(media.height);
    a.thumb = match (a.thumb.take(), media.thumb) {
        (Some(mut thumb), Some(bounded)) => {
            thumb.width = narrow(bounded.width);
            thumb.height = narrow(bounded.height);
            Some(thumb)
        },
        _ => None,
    };
}

fn bound_message(mut m: rec::VaultMessage) -> rec::VaultMessage {
    m.attachments.iter_mut().for_each(bound_media);
    m
}

fn bound_messages(messages: Vec<rec::VaultMessage>) -> Vec<VaultMessage> {
    messages
        .into_iter()
        .map(|m| VaultMessage {
            inner: bound_message(m),
        })
        .collect()
}

/// A poster-frame blob of a [`VaultAttachment`] (ADR-0047). Built with
/// `VaultAttachment.setThumb`; read here.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultThumb {
    inner: rec::VaultThumb,
}

#[wasm_bindgen]
impl VaultThumb {
    /// 32-byte blob id.
    #[wasm_bindgen(getter, js_name = blobId)]
    pub fn blob_id(&self) -> Vec<u8> {
        self.inner.blob_id.to_vec()
    }

    /// 32-byte content-encryption key.
    #[wasm_bindgen(getter)]
    pub fn key(&self) -> Vec<u8> {
        self.inner.key.to_vec()
    }

    /// 12-byte AEAD nonce.
    #[wasm_bindgen(getter)]
    pub fn nonce(&self) -> Vec<u8> {
        self.inner.nonce.to_vec()
    }

    /// Plaintext size in bytes. A `bigint`.
    #[wasm_bindgen(getter)]
    pub fn size(&self) -> u64 {
        u64::from(self.inner.size)
    }

    /// MIME type.
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> Option<String> {
        some_text(&self.inner.mime)
    }

    /// Pixel width.
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> Option<u32> {
        self.inner.width.map(u32::from)
    }

    /// Pixel height.
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> Option<u32> {
        self.inner.height.map(u32::from)
    }
}

/// An attachment reference of a saved message: where the encrypted file is
/// and how to open it. A reference, not the file, which may have expired by
/// the time the history is read (ADR-0065 section 8). The media fields are
/// ADR-0047's, bounded whenever they are set (see the module doc).
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultAttachment {
    inner: rec::VaultAttachment,
}

impl VaultAttachment {
    fn try_new(
        blob_id: &[u8],
        key: &[u8],
        nonce: &[u8],
        size: u64,
        mime: Option<String>,
        name: Option<String>,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            inner: rec::VaultAttachment {
                blob_id: fixed::<32>(blob_id, "blobIdLength", "attachment blob id")?,
                key: fixed::<32>(key, "keyLength", "attachment key")?,
                nonce: fixed::<12>(nonce, "nonceLength", "attachment nonce")?,
                size,
                mime: mime.unwrap_or_default(),
                name: name.unwrap_or_default(),
                kind: String::new(),
                duration_ms: None,
                waveform: Vec::new(),
                width: None,
                height: None,
                thumb: None,
                title: String::new(),
                artist: String::new(),
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn try_set_thumb(
        &mut self,
        blob_id: &[u8],
        key: &[u8],
        nonce: &[u8],
        size: u64,
        mime: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<(), Refusal> {
        let blob_id = fixed::<32>(blob_id, "blobIdLength", "thumb blob id")?;
        let key = fixed::<32>(key, "keyLength", "thumb key")?;
        let nonce = fixed::<12>(nonce, "nonceLength", "thumb nonce")?;
        // A size that does not fit the record is far over the ADR-0047
        // limit: the thumb is dropped, as the bound would drop it.
        self.inner.thumb = u32::try_from(size).ok().map(|size| rec::VaultThumb {
            blob_id,
            key,
            nonce,
            mime: mime.unwrap_or_default(),
            size,
            width: narrow(width),
            height: narrow(height),
        });
        bound_media(&mut self.inner);
        Ok(())
    }
}

#[wasm_bindgen]
impl VaultAttachment {
    /// `blobId` / `key` MUST be 32 bytes, `nonce` 12 bytes; `size` is the
    /// plaintext size in bytes (a `bigint`).
    ///
    /// # Errors
    ///
    /// Throws `blobIdLength: ...`, `keyLength: ...` or `nonceLength: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        blob_id: &[u8],
        key: &[u8],
        nonce: &[u8],
        size: u64,
        mime: Option<String>,
        name: Option<String>,
    ) -> Result<VaultAttachment, JsError> {
        Ok(Self::try_new(blob_id, key, nonce, size, mime, name)?)
    }

    /// ADR-0047 media metadata; replaces what was set before. `waveform` is
    /// the raw amplitude bytes. A value outside its bound is dropped, as for
    /// a live envelope: a duration is kept only for `voice` / `video_note`
    /// and within that kind's limit, a dimension only within 1..=4096.
    #[wasm_bindgen(js_name = setMedia)]
    pub fn set_media(
        &mut self,
        kind: Option<String>,
        duration_ms: Option<u32>,
        waveform: Option<Vec<u8>>,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        self.inner.kind = kind.unwrap_or_default();
        self.inner.duration_ms = duration_ms;
        self.inner.waveform = waveform.unwrap_or_default();
        self.inner.width = narrow(width);
        self.inner.height = narrow(height);
        bound_media(&mut self.inner);
    }

    /// ADR-0090 audio title and artist tags (kept on kind `"audio"` only,
    /// within 255 bytes; anything else is dropped). Call after `setMedia`.
    #[wasm_bindgen(js_name = setAudioTags)]
    pub fn set_audio_tags(&mut self, title: Option<String>, artist: Option<String>) {
        self.inner.title = title.unwrap_or_default();
        self.inner.artist = artist.unwrap_or_default();
        bound_media(&mut self.inner);
    }

    /// ADR-0090 audio title tag.
    #[wasm_bindgen(getter)]
    pub fn title(&self) -> Option<String> {
        some_text(&self.inner.title)
    }

    /// ADR-0090 audio artist tag.
    #[wasm_bindgen(getter)]
    pub fn artist(&self) -> Option<String> {
        some_text(&self.inner.artist)
    }

    /// ADR-0047 poster frame; replaces one set before. `blobId` / `key` MUST
    /// be 32 bytes, `nonce` 12 bytes. A thumb outside the size limit is
    /// dropped whole (`thumb` then reads `undefined`).
    ///
    /// # Errors
    ///
    /// Throws `blobIdLength: ...`, `keyLength: ...` or `nonceLength: ...`.
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
    ) -> Result<(), JsError> {
        Ok(self.try_set_thumb(blob_id, key, nonce, size, mime, width, height)?)
    }

    /// 32-byte blob id.
    #[wasm_bindgen(getter, js_name = blobId)]
    pub fn blob_id(&self) -> Vec<u8> {
        self.inner.blob_id.to_vec()
    }

    /// 32-byte content-encryption key.
    #[wasm_bindgen(getter)]
    pub fn key(&self) -> Vec<u8> {
        self.inner.key.to_vec()
    }

    /// 12-byte AEAD nonce.
    #[wasm_bindgen(getter)]
    pub fn nonce(&self) -> Vec<u8> {
        self.inner.nonce.to_vec()
    }

    /// Plaintext size in bytes. A `bigint`.
    #[wasm_bindgen(getter)]
    pub fn size(&self) -> u64 {
        self.inner.size
    }

    /// MIME type.
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> Option<String> {
        some_text(&self.inner.mime)
    }

    /// File name.
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> Option<String> {
        some_text(&self.inner.name)
    }

    /// Media kind (`voice`, `video_note`, `gif`, or one this build does not
    /// know); `undefined` for a plain file.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> Option<String> {
        some_text(&self.inner.kind)
    }

    /// Playback length in milliseconds.
    #[wasm_bindgen(getter, js_name = durationMs)]
    pub fn duration_ms(&self) -> Option<u32> {
        self.inner.duration_ms
    }

    /// Raw waveform amplitude bytes.
    #[wasm_bindgen(getter)]
    pub fn waveform(&self) -> Option<Vec<u8>> {
        (!self.inner.waveform.is_empty()).then(|| self.inner.waveform.clone())
    }

    /// Pixel width.
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> Option<u32> {
        self.inner.width.map(u32::from)
    }

    /// Pixel height.
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> Option<u32> {
        self.inner.height.map(u32::from)
    }

    /// Poster frame.
    #[wasm_bindgen(getter)]
    pub fn thumb(&self) -> Option<VaultThumb> {
        self.inner.thumb.clone().map(|inner| VaultThumb { inner })
    }
}

// --- messages -----------------------------------------------------------

/// One reaction of a [`VaultMessage`]. Built with `VaultMessage.addReaction`;
/// read here.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultReaction {
    inner: rec::VaultReaction,
}

#[wasm_bindgen]
impl VaultReaction {
    /// The reacting person's 32-byte account key.
    #[wasm_bindgen(getter)]
    pub fn reactor(&self) -> Vec<u8> {
        self.inner.reactor.to_vec()
    }

    /// The emoji.
    #[wasm_bindgen(getter)]
    pub fn emoji(&self) -> String {
        self.inner.emoji.clone()
    }
}

/// One saved message: the row shape of a head's tail and of every chunk.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultMessage {
    inner: rec::VaultMessage,
}

impl VaultMessage {
    fn try_new(
        message_id: u64,
        direction: u8,
        created_at_ms: i64,
        last_op_id: u64,
        text: &str,
        attachments: Vec<VaultAttachment>,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            inner: rec::VaultMessage {
                message_id,
                direction: option(direction, rec::Direction::from_wire, "direction")?,
                author: None,
                created_at_ms,
                last_op_id,
                edited: false,
                deleted: false,
                text: text.to_string(),
                attachments: attachments.into_iter().map(|a| a.inner).collect(),
                reply: None,
                forward: None,
                reactions: Vec::new(),
                call: None,
                unsupported_version: None,
                content: None,
            },
        })
    }

    fn try_set_author(&mut self, author: &[u8]) -> Result<(), Refusal> {
        self.inner.author = Some(fixed::<32>(
            author,
            "accountKeyLength",
            "author account key",
        )?);
        Ok(())
    }

    fn try_add_reaction(&mut self, reactor: &[u8], emoji: &str) -> Result<(), Refusal> {
        self.inner.reactions.push(rec::VaultReaction {
            reactor: fixed::<32>(reactor, "accountKeyLength", "reactor account key")?,
            emoji: emoji.to_string(),
        });
        Ok(())
    }
}

#[wasm_bindgen]
impl VaultMessage {
    /// `messageId` is the Delivery Service journal id (the merge key);
    /// `direction` `0` outgoing / `1` incoming / `2` system; `createdAtMs`
    /// unix milliseconds; `lastOpId` the journal id of the newest edit,
    /// delete or reaction applied to the row, or `messageId` when none was
    /// (it decides a merge); `attachments` an `Array<VaultAttachment>`, at
    /// most 64, consumed. The three ids and the time are `bigint`.
    ///
    /// # Errors
    ///
    /// Throws `unknownOption: ...` for a `direction` outside `0..=2`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        message_id: u64,
        direction: u8,
        created_at_ms: i64,
        last_op_id: u64,
        text: &str,
        attachments: Vec<VaultAttachment>,
    ) -> Result<VaultMessage, JsError> {
        Ok(Self::try_new(
            message_id,
            direction,
            created_at_ms,
            last_op_id,
            text,
            attachments,
        )?)
    }

    /// The author's 32-byte account key.
    ///
    /// # Errors
    ///
    /// Throws `accountKeyLength: ...`.
    #[wasm_bindgen(js_name = setAuthor)]
    pub fn set_author(&mut self, author: &[u8]) -> Result<(), JsError> {
        Ok(self.try_set_author(author)?)
    }

    /// Whether the text was edited, and whether the message was deleted (the
    /// row is kept, emptied).
    #[wasm_bindgen(js_name = setFlags)]
    pub fn set_flags(&mut self, edited: bool, deleted: bool) {
        self.inner.edited = edited;
        self.inner.deleted = deleted;
    }

    /// The quote of a reply (ADR-0052): the quoted message's id (a `bigint`)
    /// and the author / excerpt snapshots, each empty when none.
    #[wasm_bindgen(js_name = setReply)]
    pub fn set_reply(&mut self, target: u64, author: &str, excerpt: &str) {
        self.inner.reply = Some(rec::VaultReply {
            target,
            author: author.to_string(),
            excerpt: excerpt.to_string(),
        });
    }

    /// The origin of a forwarded message (ADR-0052): the original author's
    /// display name (empty when none) and, when the forwarder held it, the
    /// original's location. The location is kept only as a whole: both
    /// `originGroupId` (1..=128 bytes) and `originMessageId` (a `bigint`),
    /// or neither.
    #[wasm_bindgen(js_name = setForward)]
    pub fn set_forward(
        &mut self,
        author: &str,
        origin_group_id: Option<Vec<u8>>,
        origin_message_id: Option<u64>,
    ) {
        self.inner.forward = Some(rec::VaultForward {
            author: author.to_string(),
            origin: origin_group_id
                .zip(origin_message_id)
                .map(|(group_id, message_id)| rec::VaultForwardOrigin {
                    group_id,
                    message_id,
                }),
        });
    }

    /// Add one reaction: the reacting person's 32-byte account key and the
    /// emoji (1..=64 bytes).
    ///
    /// # Errors
    ///
    /// Throws `accountKeyLength: ...`.
    #[wasm_bindgen(js_name = addReaction)]
    pub fn add_reaction(&mut self, reactor: &[u8], emoji: &str) -> Result<(), JsError> {
        Ok(self.try_add_reaction(reactor, emoji)?)
    }

    /// The outcome of a call row: `outcome` as the clients number it (opaque
    /// to the codec), whether it was a video call, and its length in
    /// seconds.
    #[wasm_bindgen(js_name = setCall)]
    pub fn set_call(&mut self, outcome: u8, video: bool, duration_secs: u32) {
        self.inner.call = Some(rec::VaultCall {
            outcome,
            video,
            duration_secs,
        });
    }

    /// Mark the message as one of a wire version the saving device could not
    /// read: that version.
    #[wasm_bindgen(js_name = setUnsupportedVersion)]
    pub fn set_unsupported_version(&mut self, version: u32) {
        self.inner.unsupported_version = Some(version);
    }

    /// ADR-0090 — set the `ContentMessage` of a poll / checklist / location /
    /// contact card / article row (`undefined` clears it). Set the
    /// attachments first: an article's images are checked against them only
    /// on read.
    ///
    /// # Errors
    ///
    /// Throws a content code (`state.mismatch`, `<field>.tooLong`, ...).
    #[wasm_bindgen(js_name = setContent)]
    pub fn set_content(
        &mut self,
        content: Option<crate::content::JsContentMessage>,
    ) -> Result<(), JsError> {
        self.inner.content = match content {
            None => None,
            Some(c) => {
                crate::content::content_message_from_js(&c)?.map(|(kind, payload, state)| {
                    rec::VaultContent {
                        kind,
                        payload,
                        state,
                    }
                })
            },
        };
        Ok(())
    }

    /// ADR-0090 — the `ContentMessage`, or `undefined` (also when its
    /// payload does not decode; a state that does not belong is dropped).
    #[wasm_bindgen(getter, unchecked_return_type = "ContentMessage | undefined")]
    pub fn content(&self) -> JsValue {
        let mimes: Vec<Option<String>> = self
            .inner
            .attachments
            .iter()
            .map(|a| some_text(&a.mime))
            .collect();
        self.inner.content.as_ref().map_or(JsValue::UNDEFINED, |c| {
            crate::content::content_message_to_js(c.kind, &c.payload, &c.state, &mimes)
        })
    }

    /// The Delivery Service journal id. A `bigint`.
    #[wasm_bindgen(getter, js_name = messageId)]
    pub fn message_id(&self) -> u64 {
        self.inner.message_id
    }

    /// `0` outgoing, `1` incoming, `2` system.
    #[wasm_bindgen(getter)]
    pub fn direction(&self) -> u8 {
        self.inner.direction.to_wire()
    }

    /// The author's 32-byte account key, when known.
    #[wasm_bindgen(getter)]
    pub fn author(&self) -> Option<Vec<u8>> {
        self.inner.author.map(|a| a.to_vec())
    }

    /// When it was sent, unix milliseconds. A `bigint`.
    #[wasm_bindgen(getter, js_name = createdAtMs)]
    pub fn created_at_ms(&self) -> i64 {
        self.inner.created_at_ms
    }

    /// Journal id of the newest operation applied to the row. A `bigint`.
    #[wasm_bindgen(getter, js_name = lastOpId)]
    pub fn last_op_id(&self) -> u64 {
        self.inner.last_op_id
    }

    /// The text was edited.
    #[wasm_bindgen(getter)]
    pub fn edited(&self) -> bool {
        self.inner.edited
    }

    /// The message was deleted.
    #[wasm_bindgen(getter)]
    pub fn deleted(&self) -> bool {
        self.inner.deleted
    }

    /// The text.
    #[wasm_bindgen(getter)]
    pub fn text(&self) -> String {
        self.inner.text.clone()
    }

    /// `Array<VaultAttachment>`.
    #[wasm_bindgen(getter)]
    pub fn attachments(&self) -> Vec<VaultAttachment> {
        self.inner
            .attachments
            .iter()
            .cloned()
            .map(|inner| VaultAttachment { inner })
            .collect()
    }

    /// The quoted message's id, for a reply; `undefined` otherwise. A
    /// `bigint`.
    #[wasm_bindgen(getter, js_name = replyTarget)]
    pub fn reply_target(&self) -> Option<u64> {
        self.inner.reply.as_ref().map(|r| r.target)
    }

    /// The quoted author's display name (empty when none), for a reply.
    #[wasm_bindgen(getter, js_name = replyAuthor)]
    pub fn reply_author(&self) -> Option<String> {
        self.inner.reply.as_ref().map(|r| r.author.clone())
    }

    /// The quoted excerpt (empty when none), for a reply.
    #[wasm_bindgen(getter, js_name = replyExcerpt)]
    pub fn reply_excerpt(&self) -> Option<String> {
        self.inner.reply.as_ref().map(|r| r.excerpt.clone())
    }

    /// Whether the message was forwarded.
    #[wasm_bindgen(getter)]
    pub fn forwarded(&self) -> bool {
        self.inner.forward.is_some()
    }

    /// The original author's display name (empty when none), for a forward.
    #[wasm_bindgen(getter, js_name = forwardAuthor)]
    pub fn forward_author(&self) -> Option<String> {
        self.inner.forward.as_ref().map(|f| f.author.clone())
    }

    /// The origin conversation of a forward, when the forwarder held it.
    #[wasm_bindgen(getter, js_name = forwardOriginGroupId)]
    pub fn forward_origin_group_id(&self) -> Option<Vec<u8>> {
        let origin = self.inner.forward.as_ref()?.origin.as_ref()?;
        Some(origin.group_id.clone())
    }

    /// The original's message id in its conversation, beside
    /// `forwardOriginGroupId`. A `bigint`.
    #[wasm_bindgen(getter, js_name = forwardOriginMessageId)]
    pub fn forward_origin_message_id(&self) -> Option<u64> {
        let origin = self.inner.forward.as_ref()?.origin.as_ref()?;
        Some(origin.message_id)
    }

    /// `Array<VaultReaction>`.
    #[wasm_bindgen(getter)]
    pub fn reactions(&self) -> Vec<VaultReaction> {
        self.inner
            .reactions
            .iter()
            .cloned()
            .map(|inner| VaultReaction { inner })
            .collect()
    }

    /// The outcome of a call row; `undefined` for any other row.
    #[wasm_bindgen(getter, js_name = callOutcome)]
    pub fn call_outcome(&self) -> Option<u8> {
        self.inner.call.map(|c| c.outcome)
    }

    /// Whether a call row was a video call.
    #[wasm_bindgen(getter, js_name = callVideo)]
    pub fn call_video(&self) -> Option<bool> {
        self.inner.call.map(|c| c.video)
    }

    /// How long a call row lasted, in seconds.
    #[wasm_bindgen(getter, js_name = callDurationSecs)]
    pub fn call_duration_secs(&self) -> Option<u32> {
        self.inner.call.map(|c| c.duration_secs)
    }

    /// The wire version the saving device could not read, when that is why
    /// the row is empty.
    #[wasm_bindgen(getter, js_name = unsupportedVersion)]
    pub fn unsupported_version(&self) -> Option<u32> {
        self.inner.unsupported_version
    }
}

fn into_core_messages(messages: Vec<VaultMessage>) -> Vec<rec::VaultMessage> {
    messages.into_iter().map(|m| m.inner).collect()
}

// --- head rows ----------------------------------------------------------

/// One person in a conversation, as shown at save time. Never a device leaf.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPerson {
    inner: rec::VaultPerson,
}

impl VaultPerson {
    fn try_new(
        account_key: &[u8],
        display_name: &str,
        username: &str,
        role: u8,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            inner: rec::VaultPerson {
                account_key: fixed::<32>(account_key, "accountKeyLength", "person account key")?,
                display_name: display_name.to_string(),
                username: username.to_string(),
                role: option(role, rec::Role::from_wire, "role")?,
            },
        })
    }
}

#[wasm_bindgen]
impl VaultPerson {
    /// `accountKey` is the person's 32-byte account key; `displayName` and
    /// `username` are snapshots, each empty when none; `role` is `0` member,
    /// `1` admin, `2` owner.
    ///
    /// # Errors
    ///
    /// Throws `accountKeyLength: ...` or `unknownOption: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        account_key: &[u8],
        display_name: &str,
        username: &str,
        role: u8,
    ) -> Result<VaultPerson, JsError> {
        Ok(Self::try_new(account_key, display_name, username, role)?)
    }

    /// The person's 32-byte account key.
    #[wasm_bindgen(getter, js_name = accountKey)]
    pub fn account_key(&self) -> Vec<u8> {
        self.inner.account_key.to_vec()
    }

    /// Display-name snapshot; empty when none.
    #[wasm_bindgen(getter, js_name = displayName)]
    pub fn display_name(&self) -> String {
        self.inner.display_name.clone()
    }

    /// Username snapshot; empty when none.
    #[wasm_bindgen(getter)]
    pub fn username(&self) -> String {
        self.inner.username.clone()
    }

    /// `0` member, `1` admin, `2` owner.
    #[wasm_bindgen(getter)]
    pub fn role(&self) -> u8 {
        self.inner.role.to_wire()
    }
}

/// A pin shared with the conversation (ADR-0052). An unpin is kept, so a
/// stale pin cannot bring it back.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultSharedPin {
    inner: rec::SharedPin,
}

#[wasm_bindgen]
impl VaultSharedPin {
    /// The pinned message, pinned or unpinned, and the stamp of that state
    /// in unix milliseconds. The id and the stamp are `bigint`.
    #[wasm_bindgen(constructor)]
    pub fn new(message_id: u64, pinned: bool, updated_at_ms: u64) -> VaultSharedPin {
        Self {
            inner: rec::SharedPin {
                message_id,
                pinned,
                updated_at_ms,
            },
        }
    }

    /// The pinned message. A `bigint`.
    #[wasm_bindgen(getter, js_name = messageId)]
    pub fn message_id(&self) -> u64 {
        self.inner.message_id
    }

    /// Pinned, or unpinned.
    #[wasm_bindgen(getter)]
    pub fn pinned(&self) -> bool {
        self.inner.pinned
    }

    /// The stamp of this state, unix milliseconds. A `bigint`.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }
}

/// A pin of this account alone.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultPersonalPin {
    inner: rec::PersonalPin,
}

#[wasm_bindgen]
impl VaultPersonalPin {
    /// The pinned message and when it was pinned, unix milliseconds. Both
    /// `bigint`.
    #[wasm_bindgen(constructor)]
    pub fn new(message_id: u64, pinned_at_ms: u64) -> VaultPersonalPin {
        Self {
            inner: rec::PersonalPin {
                message_id,
                pinned_at_ms,
            },
        }
    }

    /// The pinned message. A `bigint`.
    #[wasm_bindgen(getter, js_name = messageId)]
    pub fn message_id(&self) -> u64 {
        self.inner.message_id
    }

    /// When it was pinned, unix milliseconds. A `bigint`.
    #[wasm_bindgen(getter, js_name = pinnedAtMs)]
    pub fn pinned_at_ms(&self) -> u64 {
        self.inner.pinned_at_ms
    }
}

/// One row of a head's chunk table.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultChunkEntry {
    inner: rec::ChunkEntry,
}

#[wasm_bindgen]
impl VaultChunkEntry {
    /// How many messages the chunk holds, its lowest and highest message
    /// ids, and the chunk object's current version on the storage service.
    /// The ids and the version are `bigint`.
    #[wasm_bindgen(constructor)]
    pub fn new(count: u16, first_id: u64, last_id: u64, version: u64) -> VaultChunkEntry {
        Self {
            inner: rec::ChunkEntry {
                count,
                first_id,
                last_id,
                version,
            },
        }
    }

    /// How many messages the chunk holds.
    #[wasm_bindgen(getter)]
    pub fn count(&self) -> u16 {
        self.inner.count
    }

    /// Its lowest message id. A `bigint`.
    #[wasm_bindgen(getter, js_name = firstId)]
    pub fn first_id(&self) -> u64 {
        self.inner.first_id
    }

    /// Its highest message id. A `bigint`.
    #[wasm_bindgen(getter, js_name = lastId)]
    pub fn last_id(&self) -> u64 {
        self.inner.last_id
    }

    /// The chunk object's current version. A `bigint`.
    #[wasm_bindgen(getter)]
    pub fn version(&self) -> u64 {
        self.inner.version
    }
}

/// Re-entry material for one epoch (ADR-0064 section 4).
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultReentryEpoch {
    inner: rec::ReentryEpoch,
}

#[wasm_bindgen]
impl VaultReentryEpoch {
    /// The epoch (a `bigint`) and the GroupInfo with its ratchet tree, as
    /// MLS encodes it: opaque to the codec, 1 byte to 1 MiB.
    #[wasm_bindgen(constructor)]
    pub fn new(epoch: u64, group_info: &[u8]) -> VaultReentryEpoch {
        Self {
            inner: rec::ReentryEpoch {
                epoch,
                group_info: group_info.to_vec(),
            },
        }
    }

    /// The epoch the GroupInfo describes. A `bigint`.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch
    }

    /// The GroupInfo with its ratchet tree.
    #[wasm_bindgen(getter, js_name = groupInfo)]
    pub fn group_info(&self) -> Vec<u8> {
        self.inner.group_info.clone()
    }
}

fn into_core_reentry(reentry: Vec<VaultReentryEpoch>) -> Vec<rec::ReentryEpoch> {
    reentry.into_iter().map(|e| e.inner).collect()
}

fn from_core_reentry(reentry: &[rec::ReentryEpoch]) -> Vec<VaultReentryEpoch> {
    reentry
        .iter()
        .cloned()
        .map(|inner| VaultReentryEpoch { inner })
        .collect()
}

// --- head ---------------------------------------------------------------

/// A conversation's head. See the module doc for how it is built and read.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultConversationHead {
    inner: rec::ConversationHead,
}

impl VaultConversationHead {
    fn try_new(
        ref_kind: u8,
        ref_id: &[u8],
        kind: u8,
        title: &str,
        membership: u8,
        last_activity_ms: u64,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            inner: rec::ConversationHead {
                conversation: conversation(ref_kind, ref_id)?,
                kind: option(kind, rec::ConversationKind::from_wire, "conversation kind")?,
                title: title.to_string(),
                people: Vec::new(),
                membership: option(membership, rec::Membership::from_wire, "membership")?,
                shared_pins: Vec::new(),
                personal_pins: Vec::new(),
                last_activity_ms,
                chunks: Vec::new(),
                tail: Vec::new(),
                reentry: Vec::new(),
                registrar_seed: None,
            },
        })
    }

    fn try_set_registrar_seed(&mut self, rank: u8, seed: &[u8]) -> Result<(), Refusal> {
        self.inner.registrar_seed = Some(rec::RegistrarSeed {
            rank,
            seed: SecretBytes::new(fixed::<32>(seed, "seedLength", "registrar seed")?),
        });
        Ok(())
    }

    fn try_decode(bytes: &[u8]) -> Result<Self, Refusal> {
        let mut inner = rec::ConversationHead::decode(bytes)?;
        inner.tail = inner.tail.into_iter().map(bound_message).collect();
        Ok(Self { inner })
    }

    fn try_encode(&self) -> Result<Vec<u8>, Refusal> {
        Ok(self.inner.encode()?)
    }
}

#[wasm_bindgen]
impl VaultConversationHead {
    /// `refKind` / `refId` name the conversation the ADR-0058 way (`1`
    /// one-to-one, `2` group, `3` channel; 1..=64 bytes); `kind` is `0`
    /// unknown, `1` one-to-one, `2` group (ADR-0062); `title` the group name,
    /// empty for a one-to-one or an unnamed group; `membership` `0` in, `1`
    /// left, `2` removed; `lastActivityMs` unix milliseconds (a `bigint`).
    /// Everything else starts empty; add it with the setters.
    ///
    /// # Errors
    ///
    /// Throws `unknownKind: ...`, `idLength: ...` or `unknownOption: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        ref_kind: u8,
        ref_id: &[u8],
        kind: u8,
        title: &str,
        membership: u8,
        last_activity_ms: u64,
    ) -> Result<VaultConversationHead, JsError> {
        Ok(Self::try_new(
            ref_kind,
            ref_id,
            kind,
            title,
            membership,
            last_activity_ms,
        )?)
    }

    /// The people, an `Array<VaultPerson>` of at most `vaultMaxPeople()`,
    /// consumed.
    #[wasm_bindgen(js_name = setPeople)]
    pub fn set_people(&mut self, people: Vec<VaultPerson>) {
        self.inner.people = people.into_iter().map(|p| p.inner).collect();
    }

    /// Shared pins, an `Array<VaultSharedPin>` of at most 1024, consumed.
    #[wasm_bindgen(js_name = setSharedPins)]
    pub fn set_shared_pins(&mut self, pins: Vec<VaultSharedPin>) {
        self.inner.shared_pins = pins.into_iter().map(|p| p.inner).collect();
    }

    /// Personal pins, an `Array<VaultPersonalPin>` of at most 1024, consumed.
    #[wasm_bindgen(js_name = setPersonalPins)]
    pub fn set_personal_pins(&mut self, pins: Vec<VaultPersonalPin>) {
        self.inner.personal_pins = pins.into_iter().map(|p| p.inner).collect();
    }

    /// The chunk table, an `Array<VaultChunkEntry>` of at most 65 536,
    /// consumed.
    #[wasm_bindgen(js_name = setChunks)]
    pub fn set_chunks(&mut self, chunks: Vec<VaultChunkEntry>) {
        self.inner.chunks = chunks.into_iter().map(|c| c.inner).collect();
    }

    /// The newest messages, not yet moved into a chunk: an
    /// `Array<VaultMessage>` of at most `vaultMaxTailMessages()`, consumed.
    #[wasm_bindgen(js_name = setTail)]
    pub fn set_tail(&mut self, tail: Vec<VaultMessage>) {
        self.inner.tail = into_core_messages(tail);
    }

    /// Re-entry material for the newest epochs: an `Array<VaultReentryEpoch>`
    /// of at most `vaultMaxReentryEpochs()`, consumed. Empty when the account
    /// is not in the conversation.
    #[wasm_bindgen(js_name = setReentry)]
    pub fn set_reentry(&mut self, reentry: Vec<VaultReentryEpoch>) {
        self.inner.reentry = into_core_reentry(reentry);
    }

    /// The account's registrar seed for this journal (ADR-0057), when it
    /// holds one: the rank it was issued at (opaque to the codec) and the
    /// 32-byte seed.
    ///
    /// # Errors
    ///
    /// Throws `seedLength: ...`.
    #[wasm_bindgen(js_name = setRegistrarSeed)]
    pub fn set_registrar_seed(&mut self, rank: u8, seed: &[u8]) -> Result<(), JsError> {
        Ok(self.try_set_registrar_seed(rank, seed)?)
    }

    /// The conversation's reference kind: `1` one-to-one, `2` group, `3`
    /// channel.
    #[wasm_bindgen(getter, js_name = refKind)]
    pub fn ref_kind(&self) -> u8 {
        self.inner.conversation.kind().to_wire()
    }

    /// The MLS group id, or the channel id.
    #[wasm_bindgen(getter, js_name = refId)]
    pub fn ref_id(&self) -> Vec<u8> {
        self.inner.conversation.id().to_vec()
    }

    /// `0` unknown, `1` one-to-one, `2` group.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> u8 {
        self.inner.kind.to_wire()
    }

    /// The group name; empty for a one-to-one or an unnamed group.
    #[wasm_bindgen(getter)]
    pub fn title(&self) -> String {
        self.inner.title.clone()
    }

    /// `Array<VaultPerson>`.
    #[wasm_bindgen(getter)]
    pub fn people(&self) -> Vec<VaultPerson> {
        self.inner
            .people
            .iter()
            .cloned()
            .map(|inner| VaultPerson { inner })
            .collect()
    }

    /// `0` in, `1` left, `2` removed.
    #[wasm_bindgen(getter)]
    pub fn membership(&self) -> u8 {
        self.inner.membership.to_wire()
    }

    /// `Array<VaultSharedPin>`.
    #[wasm_bindgen(getter, js_name = sharedPins)]
    pub fn shared_pins(&self) -> Vec<VaultSharedPin> {
        self.inner
            .shared_pins
            .iter()
            .map(|inner| VaultSharedPin { inner: *inner })
            .collect()
    }

    /// `Array<VaultPersonalPin>`.
    #[wasm_bindgen(getter, js_name = personalPins)]
    pub fn personal_pins(&self) -> Vec<VaultPersonalPin> {
        self.inner
            .personal_pins
            .iter()
            .map(|inner| VaultPersonalPin { inner: *inner })
            .collect()
    }

    /// Last activity, unix milliseconds. A `bigint`.
    #[wasm_bindgen(getter, js_name = lastActivityMs)]
    pub fn last_activity_ms(&self) -> u64 {
        self.inner.last_activity_ms
    }

    /// `Array<VaultChunkEntry>`.
    #[wasm_bindgen(getter)]
    pub fn chunks(&self) -> Vec<VaultChunkEntry> {
        self.inner
            .chunks
            .iter()
            .map(|inner| VaultChunkEntry { inner: *inner })
            .collect()
    }

    /// `Array<VaultMessage>`: the newest messages, not yet in a chunk.
    #[wasm_bindgen(getter)]
    pub fn tail(&self) -> Vec<VaultMessage> {
        self.inner
            .tail
            .iter()
            .cloned()
            .map(|inner| VaultMessage { inner })
            .collect()
    }

    /// `Array<VaultReentryEpoch>`.
    #[wasm_bindgen(getter)]
    pub fn reentry(&self) -> Vec<VaultReentryEpoch> {
        from_core_reentry(&self.inner.reentry)
    }

    /// The rank the account's registrar seed was issued at, when the head
    /// carries one.
    #[wasm_bindgen(getter, js_name = registrarRank)]
    pub fn registrar_rank(&self) -> Option<u8> {
        self.inner.registrar_seed.as_ref().map(|r| r.rank)
    }

    /// The account's 32-byte registrar seed for this journal, when the head
    /// carries one.
    ///
    /// **Secret.** It crosses this boundary as bytes because the client
    /// already holds it that way: it stores the one it receives in-group
    /// (`openRegistrarSeedEnvelope`) wrapped under the device key, and it
    /// must store one read from a head the same way and nowhere else. Never
    /// log it.
    #[wasm_bindgen(getter, js_name = registrarSeed)]
    pub fn registrar_seed(&self) -> Option<Vec<u8>> {
        self.inner
            .registrar_seed
            .as_ref()
            .map(|r| r.seed.as_bytes().to_vec())
    }
}

/// One conversation in the index.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultIndexEntry {
    inner: rec::IndexEntry,
}

impl VaultIndexEntry {
    fn try_new(
        ref_kind: u8,
        ref_id: &[u8],
        last_activity_ms: u64,
        membership: u8,
    ) -> Result<Self, Refusal> {
        Ok(Self {
            inner: rec::IndexEntry {
                conversation: conversation(ref_kind, ref_id)?,
                last_activity_ms,
                membership: option(membership, rec::Membership::from_wire, "membership")?,
            },
        })
    }
}

#[wasm_bindgen]
impl VaultIndexEntry {
    /// `refKind` / `refId` name the conversation; `lastActivityMs` (unix
    /// milliseconds, a `bigint`) orders the loading; `membership` is `0` in,
    /// `1` left, `2` removed.
    ///
    /// # Errors
    ///
    /// Throws `unknownKind: ...`, `idLength: ...` or `unknownOption: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        ref_kind: u8,
        ref_id: &[u8],
        last_activity_ms: u64,
        membership: u8,
    ) -> Result<VaultIndexEntry, JsError> {
        Ok(Self::try_new(
            ref_kind,
            ref_id,
            last_activity_ms,
            membership,
        )?)
    }

    /// `1` one-to-one, `2` group, `3` channel.
    #[wasm_bindgen(getter, js_name = refKind)]
    pub fn ref_kind(&self) -> u8 {
        self.inner.conversation.kind().to_wire()
    }

    /// The MLS group id, or the channel id.
    #[wasm_bindgen(getter, js_name = refId)]
    pub fn ref_id(&self) -> Vec<u8> {
        self.inner.conversation.id().to_vec()
    }

    /// Last activity, unix milliseconds. A `bigint`.
    #[wasm_bindgen(getter, js_name = lastActivityMs)]
    pub fn last_activity_ms(&self) -> u64 {
        self.inner.last_activity_ms
    }

    /// `0` in, `1` left, `2` removed.
    #[wasm_bindgen(getter)]
    pub fn membership(&self) -> u8 {
        self.inner.membership.to_wire()
    }
}

// --- record functions ---------------------------------------------------

fn try_encode_history_chunk(messages: Vec<VaultMessage>) -> Result<Vec<u8>, Refusal> {
    let chunk = rec::HistoryChunk {
        messages: into_core_messages(messages),
    };
    Ok(chunk.encode()?)
}

fn try_decode_history_chunk(bytes: &[u8]) -> Result<Vec<VaultMessage>, Refusal> {
    Ok(bound_messages(rec::HistoryChunk::decode(bytes)?.messages))
}

fn try_encode_vault_index(entries: Vec<VaultIndexEntry>) -> Result<Vec<u8>, Refusal> {
    let index = rec::VaultIndex {
        entries: entries.into_iter().map(|e| e.inner).collect(),
    };
    Ok(index.encode()?)
}

fn try_decode_vault_index(bytes: &[u8]) -> Result<Vec<VaultIndexEntry>, Refusal> {
    Ok(rec::VaultIndex::decode(bytes)?
        .entries
        .into_iter()
        .map(|inner| VaultIndexEntry { inner })
        .collect())
}

fn try_encode_devices_head(reentry: Vec<VaultReentryEpoch>) -> Result<Vec<u8>, Refusal> {
    let head = rec::DevicesHead {
        reentry: into_core_reentry(reentry),
    };
    Ok(head.encode()?)
}

fn try_decode_devices_head(bytes: &[u8]) -> Result<Vec<VaultReentryEpoch>, Refusal> {
    Ok(from_core_reentry(&rec::DevicesHead::decode(bytes)?.reentry))
}

/// Encode a conversation's head: the plaintext `VaultKeys.sealHead` takes.
/// The head is read, not consumed.
///
/// # Errors
///
/// Throws a `RecordError` bound (`title`, `people`, `pins`, `chunks`,
/// `messages`, `text`, `attachments`, `attachment`, `reply`, `forward`,
/// `reactions`, `reentry`), or `tooLarge` when the encoding does not fit one
/// object - move messages from the tail into chunks and encode again.
#[wasm_bindgen(js_name = encodeConversationHead)]
pub fn encode_conversation_head(head: &VaultConversationHead) -> Result<Vec<u8>, JsError> {
    Ok(head.try_encode()?)
}

/// Decode what `VaultKeys.openHead` returned for a conversation's vault. The
/// `registrarSeed` of the result is secret.
///
/// # Errors
///
/// Throws the `RecordError` code of the first thing that is wrong
/// (`unknownVersion`, `truncated`, `trailingBytes`, a bound, ...).
#[wasm_bindgen(js_name = decodeConversationHead)]
pub fn decode_conversation_head(bytes: &[u8]) -> Result<VaultConversationHead, JsError> {
    Ok(VaultConversationHead::try_decode(bytes)?)
}

/// Encode one chunk of earlier messages (an `Array<VaultMessage>` of at most
/// `vaultMaxChunkMessages()`, consumed): the plaintext `VaultKeys.sealChunk`
/// takes. The codec asks for no order; `mergeVaultMessages` is what yields
/// rows sorted by id, each id once.
///
/// # Errors
///
/// Throws a message bound, or `tooLarge` when the encoding does not fit one
/// object - split the chunk.
#[wasm_bindgen(js_name = encodeHistoryChunk)]
pub fn encode_history_chunk(messages: Vec<VaultMessage>) -> Result<Vec<u8>, JsError> {
    Ok(try_encode_history_chunk(messages)?)
}

/// Decode what `VaultKeys.openChunk` returned into its messages.
///
/// # Errors
///
/// Throws the `RecordError` code of the first thing that is wrong.
#[wasm_bindgen(js_name = decodeHistoryChunk)]
pub fn decode_history_chunk(bytes: &[u8]) -> Result<Vec<VaultMessage>, JsError> {
    Ok(try_decode_history_chunk(bytes)?)
}

/// Encode the account's index (an `Array<VaultIndexEntry>` of at most
/// 65 536, no conversation twice, consumed): the plaintext of the head of the
/// `VaultRef.index()` vault.
///
/// # Errors
///
/// Throws `entries: ...`, `repeatedRef: ...` or `tooLarge: ...`.
#[wasm_bindgen(js_name = encodeVaultIndex)]
pub fn encode_vault_index(entries: Vec<VaultIndexEntry>) -> Result<Vec<u8>, JsError> {
    Ok(try_encode_vault_index(entries)?)
}

/// Decode the head of the `VaultRef.index()` vault into its entries.
///
/// # Errors
///
/// Throws the `RecordError` code of the first thing that is wrong.
#[wasm_bindgen(js_name = decodeVaultIndex)]
pub fn decode_vault_index(bytes: &[u8]) -> Result<Vec<VaultIndexEntry>, JsError> {
    Ok(try_decode_vault_index(bytes)?)
}

/// Encode the head of the account's own device group (ADR-0064 section 5):
/// re-entry material for at most `vaultMaxReentryEpochs()` epochs (an
/// `Array<VaultReentryEpoch>`, consumed), the plaintext of the head of the
/// `VaultRef.devices()` vault.
///
/// # Errors
///
/// Throws `reentry: ...` or `tooLarge: ...`.
#[wasm_bindgen(js_name = encodeDevicesHead)]
pub fn encode_devices_head(reentry: Vec<VaultReentryEpoch>) -> Result<Vec<u8>, JsError> {
    Ok(try_encode_devices_head(reentry)?)
}

/// Decode the head of the `VaultRef.devices()` vault into its re-entry
/// material.
///
/// # Errors
///
/// Throws the `RecordError` code of the first thing that is wrong.
#[wasm_bindgen(js_name = decodeDevicesHead)]
pub fn decode_devices_head(bytes: &[u8]) -> Result<Vec<VaultReentryEpoch>, JsError> {
    Ok(try_decode_devices_head(bytes)?)
}

/// Merge two sets of saved messages (both `Array<VaultMessage>`, consumed):
/// what a device does when its write meets `409`. The union of the rows by
/// `messageId`; for one id the row with the higher `lastOpId` wins, and on a
/// tie the same row wins on every device. The result is sorted by
/// `messageId` and holds each id once. Commutative, associative and
/// idempotent; never fails.
#[wasm_bindgen(js_name = mergeVaultMessages)]
pub fn merge_vault_messages(a: Vec<VaultMessage>, b: Vec<VaultMessage>) -> Vec<VaultMessage> {
    let a = into_core_messages(a);
    let b = into_core_messages(b);
    merge_messages(&a, &b)
        .into_iter()
        .map(|inner| VaultMessage { inner })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (Identity, VaultKeys) {
        let identity = Identity::new("account").unwrap();
        let keys = vault_keys(&identity);
        (identity, keys)
    }

    fn group(id: u8) -> VaultRef {
        VaultRef::try_conversation(2, &[id; 32]).unwrap()
    }

    fn code(refusal: &Refusal) -> &str {
        let (code, reason) = refusal.0.split_once(": ").expect("code: reason");
        assert!(!reason.is_empty());
        code
    }

    fn attachment() -> VaultAttachment {
        let mut a = VaultAttachment::try_new(
            &[1; 32],
            &[2; 32],
            &[3; 12],
            123_456,
            Some("audio/mp4".into()),
            Some("voice.m4a".into()),
        )
        .unwrap();
        a.set_media(
            Some("voice".into()),
            Some(7_250),
            Some(vec![10, 20, 30]),
            Some(640),
            Some(480),
        );
        a.try_set_thumb(
            &[4; 32],
            &[5; 32],
            &[6; 12],
            5_000,
            Some("image/jpeg".into()),
            Some(240),
            Some(180),
        )
        .unwrap();
        a
    }

    /// A message carrying every optional field.
    fn full_message(message_id: u64, last_op_id: u64) -> VaultMessage {
        let mut m = VaultMessage::try_new(
            message_id,
            1,
            1_790_000_000_123,
            last_op_id,
            "hello",
            vec![attachment()],
        )
        .unwrap();
        m.try_set_author(&[0xA1; 32]).unwrap();
        m.set_flags(true, false);
        m.set_reply(3, "Ann", "earlier");
        m.set_forward("Bob", Some(vec![0xF0; 32]), Some(9));
        m.try_add_reaction(&[0xB2; 32], "+1").unwrap();
        m.set_call(2, true, 61);
        m.set_unsupported_version(7);
        m
    }

    fn plain_message(message_id: u64, last_op_id: u64, text: &str) -> VaultMessage {
        VaultMessage::try_new(message_id, 0, 0, last_op_id, text, vec![]).unwrap()
    }

    fn head() -> VaultConversationHead {
        let mut head =
            VaultConversationHead::try_new(2, &[0xA7; 32], 2, "Friends", 0, 1_790_000_000_999)
                .unwrap();
        head.set_people(vec![
            VaultPerson::try_new(&[0xA1; 32], "Ann", "ann", 2).unwrap()
        ]);
        head.set_shared_pins(vec![VaultSharedPin::new(4, true, 99)]);
        head.set_personal_pins(vec![VaultPersonalPin::new(5, 100)]);
        head.set_chunks(vec![VaultChunkEntry::new(512, 1, 600, 3)]);
        head.set_tail(vec![
            full_message(601, 650),
            plain_message(602, 602, "tail"),
        ]);
        head.set_reentry(vec![
            VaultReentryEpoch::new(11, &[0x11; 40]),
            VaultReentryEpoch::new(12, &[0x12; 40]),
        ]);
        head.try_set_registrar_seed(2, &[0x5E; 32]).unwrap();
        head
    }

    #[test]
    fn keys_are_stable_and_distinct_per_ref() {
        let (identity, keys) = keys();
        let again = vault_keys(&identity);
        let refs = [
            group(1),
            group(2),
            VaultRef::try_conversation(1, &[1; 32]).unwrap(),
            VaultRef::try_conversation(3, &[1; 32]).unwrap(),
            VaultRef::index(),
            VaultRef::devices(),
        ];
        let mut seen = Vec::new();
        for vault in &refs {
            let id = keys.vault_id(vault);
            let public = keys.write_public_key(vault);
            assert_eq!(id.len(), 32);
            assert_eq!(public.len(), 32);
            assert_eq!(id, again.vault_id(vault));
            assert_eq!(public, again.write_public_key(vault));
            assert_ne!(id, public);
            assert!(!seen.contains(&id));
            assert!(!seen.contains(&public));
            seen.push(id);
            seen.push(public);
        }

        // The derivation is the core's, from the account identity seed.
        let root = VaultRoot::from_account_seed(&identity.mls_identity().ed25519_seed_bytes());
        assert_eq!(
            keys.vault_id(&VaultRef::index()),
            root.vault_id(&CoreVaultRef::Index)
        );

        // Another account names unrelated vaults.
        let (_, other) = self::keys();
        assert_ne!(
            keys.vault_id(&VaultRef::index()),
            other.vault_id(&VaultRef::index())
        );

        // A reference outside the ADR-0058 shape is refused, never derived.
        for (kind, id, want) in [
            (2u8, vec![], "idLength"),
            (2, vec![0u8; 65], "idLength"),
            (0, vec![1u8; 32], "unknownKind"),
            (4, vec![1u8; 32], "unknownKind"),
        ] {
            assert_eq!(
                code(&VaultRef::try_conversation(kind, &id).unwrap_err()),
                want
            );
        }
    }

    #[test]
    fn seal_and_open_round_trip_and_refuse_another_address() {
        let (_, keys) = keys();
        let vault = group(1);
        let plaintext = b"a head".to_vec();
        let head = ObjectKind::Head;
        let chunk = ObjectKind::Chunk;

        let sealed = keys.try_seal(&vault, head, 0, 7, &plaintext).unwrap();
        assert_eq!(
            u64::try_from(sealed.len()).unwrap(),
            vault_sealed_len(u64::try_from(plaintext.len()).unwrap()).unwrap()
        );
        assert_eq!(
            keys.try_open(&vault, head, 0, 7, &sealed).unwrap(),
            plaintext
        );
        // The exported pair is the same path.
        assert_eq!(
            keys.open_head(
                &vault,
                7,
                &keys.seal_head(&vault, 7, &plaintext).ok().unwrap()
            )
            .ok()
            .unwrap(),
            plaintext
        );
        // A fresh nonce each time: the same plaintext seals differently.
        assert_ne!(
            sealed,
            keys.try_seal(&vault, head, 0, 7, &plaintext).unwrap()
        );

        // Wrong version, wrong vault, wrong kind.
        for refused in [
            keys.try_open(&vault, head, 0, 8, &sealed),
            keys.try_open(&group(2), head, 0, 7, &sealed),
            keys.try_open(&VaultRef::index(), head, 0, 7, &sealed),
            keys.try_open(&vault, chunk, 0, 7, &sealed),
        ] {
            assert_eq!(code(&refused.unwrap_err()), "authentication");
        }

        let sealed_chunk = keys.try_seal(&vault, chunk, 3, 9, &plaintext).unwrap();
        assert_eq!(
            keys.try_open(&vault, chunk, 3, 9, &sealed_chunk).unwrap(),
            plaintext
        );
        assert_eq!(
            keys.open_chunk(
                &vault,
                3,
                9,
                &keys.seal_chunk(&vault, 3, 9, &plaintext).ok().unwrap()
            )
            .ok()
            .unwrap(),
            plaintext
        );
        // Wrong chunk number, wrong version, wrong kind, wrong vault.
        for refused in [
            keys.try_open(&vault, chunk, 4, 9, &sealed_chunk),
            keys.try_open(&vault, chunk, 3, 10, &sealed_chunk),
            keys.try_open(&vault, head, 0, 9, &sealed_chunk),
            keys.try_open(&group(2), chunk, 3, 9, &sealed_chunk),
        ] {
            assert_eq!(code(&refused.unwrap_err()), "authentication");
        }

        // Hostile bytes are refused with the core's codes.
        assert_eq!(
            code(&keys.try_open(&vault, head, 0, 7, &[]).unwrap_err()),
            "truncated"
        );
        let mut altered = sealed;
        altered[0] = 0x02;
        assert_eq!(
            code(&keys.try_open(&vault, head, 0, 7, &altered).unwrap_err()),
            "unknownFormat"
        );
    }

    #[test]
    fn a_head_round_trips_with_every_optional_field() {
        let original = head();
        let bytes = original.try_encode().unwrap();
        let back = VaultConversationHead::try_decode(&bytes).unwrap();
        assert_eq!(back, original);
        // Canonical: what decodes re-encodes to the identical bytes.
        assert_eq!(back.try_encode().unwrap(), bytes);

        // Sealed and opened through the keys, it is still the same head.
        let (_, keys) = keys();
        let vault = VaultRef::try_conversation(back.ref_kind(), &back.ref_id()).unwrap();
        let sealed = keys
            .try_seal(&vault, ObjectKind::Head, 0, 1, &bytes)
            .unwrap();
        let opened = keys
            .try_open(&vault, ObjectKind::Head, 0, 1, &sealed)
            .unwrap();
        assert_eq!(
            VaultConversationHead::try_decode(&opened).unwrap(),
            original
        );
    }

    #[test]
    fn a_head_reads_back_what_was_set() {
        let back = VaultConversationHead::try_decode(&head().try_encode().unwrap()).unwrap();
        assert_eq!((back.ref_kind(), back.ref_id()), (2, vec![0xA7; 32]));
        assert_eq!((back.kind(), back.membership()), (2, 0));
        assert_eq!(back.title(), "Friends");
        assert_eq!(back.last_activity_ms(), 1_790_000_000_999);
        let person = back.people().pop().unwrap();
        assert_eq!(person.account_key(), vec![0xA1; 32]);
        assert_eq!(
            (person.display_name(), person.username(), person.role()),
            ("Ann".to_string(), "ann".to_string(), 2)
        );
        let pin = back.shared_pins()[0];
        assert_eq!(
            (pin.message_id(), pin.pinned(), pin.updated_at_ms()),
            (4, true, 99)
        );
        let pin = back.personal_pins()[0];
        assert_eq!((pin.message_id(), pin.pinned_at_ms()), (5, 100));
        let entry = back.chunks()[0];
        assert_eq!(
            (
                entry.count(),
                entry.first_id(),
                entry.last_id(),
                entry.version()
            ),
            (512, 1, 600, 3)
        );
        let reentry = back.reentry();
        assert_eq!(reentry.len(), 2);
        assert_eq!(
            (reentry[1].epoch(), reentry[1].group_info()),
            (12, vec![0x12; 40])
        );
        assert_eq!(back.registrar_rank(), Some(2));
        assert_eq!(back.registrar_seed(), Some(vec![0x5E; 32]));
        assert_eq!(back.tail().len(), 2);
    }

    #[test]
    fn a_message_reads_back_what_was_set() {
        let back = VaultConversationHead::try_decode(&head().try_encode().unwrap()).unwrap();
        let tail = back.tail();
        assert_eq!(tail.len(), 2);
        let m = &tail[0];
        assert_eq!(
            (m.message_id(), m.direction(), m.last_op_id()),
            (601, 1, 650)
        );
        assert_eq!(m.author(), Some(vec![0xA1; 32]));
        assert_eq!(m.created_at_ms(), 1_790_000_000_123);
        assert_eq!((m.edited(), m.deleted()), (true, false));
        assert_eq!(m.text(), "hello");
        assert_eq!(m.reply_target(), Some(3));
        assert_eq!(m.reply_author().as_deref(), Some("Ann"));
        assert_eq!(m.reply_excerpt().as_deref(), Some("earlier"));
        assert!(m.forwarded());
        assert_eq!(m.forward_author().as_deref(), Some("Bob"));
        assert_eq!(m.forward_origin_group_id(), Some(vec![0xF0; 32]));
        assert_eq!(m.forward_origin_message_id(), Some(9));
        let reaction = m.reactions().pop().unwrap();
        assert_eq!(
            (reaction.reactor(), reaction.emoji()),
            (vec![0xB2; 32], "+1".to_string())
        );
        assert_eq!(
            (m.call_outcome(), m.call_video(), m.call_duration_secs()),
            (Some(2), Some(true), Some(61))
        );
        assert_eq!(m.unsupported_version(), Some(7));

        let a = m.attachments().pop().unwrap();
        assert_eq!(
            (a.blob_id(), a.key(), a.nonce()),
            (vec![1; 32], vec![2; 32], vec![3; 12])
        );
        assert_eq!(a.size(), 123_456);
        assert_eq!(a.mime().as_deref(), Some("audio/mp4"));
        assert_eq!(a.name().as_deref(), Some("voice.m4a"));
        assert_eq!(a.kind().as_deref(), Some("voice"));
        assert_eq!(a.duration_ms(), Some(7_250));
        assert_eq!(a.waveform(), Some(vec![10, 20, 30]));
        assert_eq!((a.width(), a.height()), (Some(640), Some(480)));
        let thumb = a.thumb().unwrap();
        assert_eq!(
            (thumb.blob_id(), thumb.key(), thumb.nonce()),
            (vec![4; 32], vec![5; 32], vec![6; 12])
        );
        assert_eq!(thumb.size(), 5_000);
        assert_eq!(thumb.mime().as_deref(), Some("image/jpeg"));
        assert_eq!((thumb.width(), thumb.height()), (Some(240), Some(180)));

        // A plain row reads every optional field as absent.
        let plain = &tail[1];
        assert_eq!(plain.author(), None);
        assert_eq!(plain.reply_target(), None);
        assert_eq!(plain.reply_author(), None);
        assert!(!plain.forwarded());
        assert_eq!(plain.forward_origin_group_id(), None);
        assert_eq!(plain.call_outcome(), None);
        assert_eq!(plain.unsupported_version(), None);
        assert!(plain.attachments().is_empty());
    }

    #[test]
    fn a_chunk_an_index_and_a_devices_head_round_trip() {
        let messages = vec![full_message(1, 5), plain_message(2, 2, "two")];
        let bytes = try_encode_history_chunk(messages.clone()).unwrap();
        assert_eq!(try_decode_history_chunk(&bytes).unwrap(), messages);

        let entries = vec![
            VaultIndexEntry::try_new(1, &[1; 32], 10, 0).unwrap(),
            VaultIndexEntry::try_new(3, &[2; 16], 20, 2).unwrap(),
        ];
        let bytes = try_encode_vault_index(entries.clone()).unwrap();
        let back = try_decode_vault_index(&bytes).unwrap();
        assert_eq!(back, entries);
        assert_eq!(
            (
                back[1].ref_kind(),
                back[1].ref_id(),
                back[1].last_activity_ms(),
                back[1].membership()
            ),
            (3, vec![2; 16], 20, 2)
        );

        let reentry = vec![VaultReentryEpoch::new(4, &[9; 12])];
        let bytes = try_encode_devices_head(reentry.clone()).unwrap();
        assert_eq!(try_decode_devices_head(&bytes).unwrap(), reentry);
    }

    #[test]
    fn refusals_carry_the_core_code() {
        // A record bound.
        let entry = VaultIndexEntry::try_new(2, &[1; 32], 1, 1).unwrap();
        assert_eq!(
            code(&try_encode_vault_index(vec![entry.clone(), entry]).unwrap_err()),
            "repeatedRef"
        );
        assert_eq!(
            code(&try_encode_devices_head(vec![VaultReentryEpoch::new(1, &[])]).unwrap_err()),
            "reentry"
        );
        // Hostile bytes.
        assert_eq!(
            code(&VaultConversationHead::try_decode(&[]).unwrap_err()),
            "truncated"
        );
        assert_eq!(
            code(&try_decode_history_chunk(&[0x01, 0, 0]).unwrap_err()),
            "unknownVersion"
        );
        let mut trailing = try_encode_devices_head(vec![]).unwrap();
        trailing.push(0);
        assert_eq!(
            code(&try_decode_devices_head(&trailing).unwrap_err()),
            "trailingBytes"
        );
        // Enum numbers outside their range.
        assert_eq!(
            code(&VaultMessage::try_new(1, 3, 0, 1, "", vec![]).unwrap_err()),
            "unknownOption"
        );
        assert_eq!(
            code(&VaultPerson::try_new(&[0; 32], "", "", 3).unwrap_err()),
            "unknownOption"
        );
        assert_eq!(
            code(&VaultConversationHead::try_new(2, &[1; 32], 3, "", 0, 0).unwrap_err()),
            "unknownOption"
        );
        assert_eq!(
            code(&VaultConversationHead::try_new(2, &[1; 32], 2, "", 3, 0).unwrap_err()),
            "unknownOption"
        );
        assert_eq!(
            code(&VaultIndexEntry::try_new(0, &[1; 32], 0, 0).unwrap_err()),
            "unknownKind"
        );
        // Argument lengths this module checks.
        let mut message = plain_message(1, 1, "x");
        assert_eq!(
            code(&message.try_set_author(&[0; 31]).unwrap_err()),
            "accountKeyLength"
        );
        assert_eq!(
            code(&message.try_add_reaction(&[0; 5], "+1").unwrap_err()),
            "accountKeyLength"
        );
        assert_eq!(
            code(&VaultPerson::try_new(&[0; 33], "", "", 0).unwrap_err()),
            "accountKeyLength"
        );
        assert_eq!(
            code(
                &VaultAttachment::try_new(&[0; 31], &[0; 32], &[0; 12], 0, None, None).unwrap_err()
            ),
            "blobIdLength"
        );
        assert_eq!(
            code(
                &VaultAttachment::try_new(&[0; 32], &[0; 16], &[0; 12], 0, None, None).unwrap_err()
            ),
            "keyLength"
        );
        assert_eq!(
            code(
                &VaultAttachment::try_new(&[0; 32], &[0; 32], &[0; 11], 0, None, None).unwrap_err()
            ),
            "nonceLength"
        );
        assert_eq!(
            code(
                &attachment()
                    .try_set_thumb(&[0; 32], &[0; 32], &[0; 13], 1, None, None, None)
                    .unwrap_err()
            ),
            "nonceLength"
        );
        assert_eq!(
            code(&head().try_set_registrar_seed(1, &[0; 33]).unwrap_err()),
            "seedLength"
        );
    }

    #[test]
    fn merge_keeps_the_later_operation_and_sorts_by_id() {
        let a = vec![plain_message(2, 2, "two"), plain_message(1, 1, "one")];
        let b = vec![plain_message(2, 9, "two, edited"), full_message(3, 3)];
        let merged = merge_vault_messages(a.clone(), b.clone());
        assert_eq!(
            merged
                .iter()
                .map(VaultMessage::message_id)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(merged[1].text(), "two, edited");
        assert_eq!(merged[2], full_message(3, 3));
        // The same answer whichever side a device holds.
        assert_eq!(merge_vault_messages(b, a), merged);
        // Idempotent.
        assert_eq!(merge_vault_messages(merged.clone(), merged.clone()), merged);
        // And what it yields encodes.
        assert!(try_encode_history_chunk(merged).is_ok());
    }

    #[test]
    fn a_signed_request_verifies_under_the_write_public_key() {
        let (_, keys) = keys();
        let vault = group(1);
        let public: [u8; 32] = keys.write_public_key(&vault).try_into().unwrap();
        let body = b"sealed head";
        let path = "/vault/00ff/head";
        let header = keys.try_sign_request(&vault, "PUT", path, body).unwrap();
        proof::verify(&public, "PUT", path, body, &header, now_ms()).unwrap();
        // The exported method signs the same way.
        let exported = keys.sign_request(&vault, "PUT", path, body).ok().unwrap();
        proof::verify(&public, "PUT", path, body, &exported, now_ms()).unwrap();
        // Another body, path or vault's key does not verify.
        assert!(proof::verify(&public, "PUT", path, b"other", &header, now_ms()).is_err());
        assert!(proof::verify(
            &public,
            "PUT",
            "/vault/00ff/chunk/0",
            body,
            &header,
            now_ms()
        )
        .is_err());
        let other: [u8; 32] = keys.write_public_key(&group(2)).try_into().unwrap();
        assert!(proof::verify(&other, "PUT", path, body, &header, now_ms()).is_err());

        assert_eq!(
            code(
                &keys
                    .try_sign_request(&vault, "PUT", "/vault/\0", &[])
                    .unwrap_err()
            ),
            "requestNotCanonical"
        );
        assert_eq!(storage_proof_header_name(), "x-lakko-storage-proof");
    }

    #[test]
    fn a_blob_delete_proof_verifies_under_the_blob_delete_public_key() {
        let (_, keys) = keys();
        let blob_id = [0x5C; 32];
        let public: [u8; 32] = keys
            .try_blob_delete_public_key(&blob_id)
            .unwrap()
            .try_into()
            .unwrap();
        let header = keys
            .try_sign_blob_delete(&blob_id, "DELETE", "/blob/5c5c", &[])
            .unwrap();
        proof::verify(&public, "DELETE", "/blob/5c5c", b"", &header, now_ms()).unwrap();

        // Another blob has an unrelated key, and neither is a vault's.
        let other = keys.try_blob_delete_public_key(&[0x5D; 32]).unwrap();
        assert_ne!(other, public.to_vec());
        let other: [u8; 32] = other.try_into().unwrap();
        assert!(proof::verify(&other, "DELETE", "/blob/5c5c", b"", &header, now_ms()).is_err());
        assert_ne!(keys.write_public_key(&VaultRef::index()), public.to_vec());

        assert_eq!(
            code(&keys.try_blob_delete_public_key(&[0; 31]).unwrap_err()),
            "blobIdLength"
        );
        assert_eq!(
            code(
                &keys
                    .try_sign_blob_delete(&[0; 33], "DELETE", "/blob/x", &[])
                    .unwrap_err()
            ),
            "blobIdLength"
        );
    }

    /// A chunk written by the core alone - as an older or a compromised
    /// device of the account could - with media metadata an ordinary
    /// received message could never carry.
    fn chunk_with_out_of_bounds_media() -> Vec<u8> {
        let attachment = |kind: &str, duration_ms: u32, thumb_size: u32| rec::VaultAttachment {
            blob_id: [1; 32],
            key: [2; 32],
            nonce: [3; 12],
            size: 1_000,
            mime: "video/mp4".into(),
            name: String::new(),
            kind: kind.into(),
            duration_ms: Some(duration_ms),
            waveform: vec![7; 3],
            width: Some(5_000),
            height: Some(480),
            thumb: Some(rec::VaultThumb {
                blob_id: [4; 32],
                key: [5; 32],
                nonce: [6; 12],
                mime: "image/jpeg".into(),
                size: thumb_size,
                width: Some(240),
                height: Some(0),
            }),
            title: String::new(),
            artist: String::new(),
        };
        let mut message = plain_message(1, 1, "").inner;
        message.attachments = vec![
            attachment("video_note", 61_000, 5_000),
            attachment("gif", 1_000, 300_000),
            attachment("voice", 7_250, 5_000),
        ];
        rec::HistoryChunk {
            messages: vec![message],
        }
        .encode()
        .unwrap()
    }

    fn assert_bounded(attachments: &[VaultAttachment]) {
        let [note, gif, voice] = attachments else {
            panic!("three attachments");
        };
        // A video note longer than a minute keeps no duration; a width
        // outside 1..=4096 is dropped alone, the height beside it is kept.
        assert_eq!(note.duration_ms(), None);
        assert_eq!((note.width(), note.height()), (None, Some(480)));
        // A thumb within its size limit is kept, its zero height dropped.
        let thumb = note.thumb().unwrap();
        assert_eq!(
            (thumb.size(), thumb.width(), thumb.height()),
            (5_000, Some(240), None)
        );
        // A gif has no duration; a thumb over the size limit goes whole.
        assert_eq!(gif.duration_ms(), None);
        assert!(gif.thumb().is_none());
        // What is within its bound survives.
        assert_eq!(voice.kind().as_deref(), Some("voice"));
        assert_eq!(voice.duration_ms(), Some(7_250));
        assert_eq!(voice.waveform(), Some(vec![7; 3]));
    }

    #[test]
    fn out_of_bounds_media_is_dropped_on_decode() {
        let bytes = chunk_with_out_of_bounds_media();
        let messages = try_decode_history_chunk(&bytes).unwrap();
        assert_bounded(&messages[0].attachments());

        // Re-encoding what the client was handed writes the bounded row.
        let rewritten = try_encode_history_chunk(messages.clone()).unwrap();
        assert_ne!(rewritten, bytes);
        assert_eq!(try_decode_history_chunk(&rewritten).unwrap(), messages);

        // The same rows in a head's tail get the same treatment.
        let mut head = head().inner;
        head.tail = rec::HistoryChunk::decode(&bytes).unwrap().messages;
        let decoded = VaultConversationHead::try_decode(&head.encode().unwrap()).unwrap();
        assert_bounded(&decoded.tail()[0].attachments());
    }

    #[test]
    fn out_of_bounds_media_is_dropped_when_it_is_set() {
        let mut a = attachment();
        // A gif has no duration; neither dimension is within 1..=4096.
        a.set_media(Some("gif".into()), Some(1_000), None, Some(70_000), Some(0));
        assert_eq!(a.kind().as_deref(), Some("gif"));
        assert_eq!(a.duration_ms(), None);
        assert_eq!(a.waveform(), None);
        assert_eq!((a.width(), a.height()), (None, None));
        // The thumb set earlier is untouched by a media change.
        assert!(a.thumb().is_some());

        // A thumb outside the size limit is dropped whole, also one whose
        // size does not fit the record at all.
        for size in [0, 300_000, u64::MAX] {
            let mut a = attachment();
            a.try_set_thumb(&[4; 32], &[5; 32], &[6; 12], size, None, None, None)
                .unwrap();
            assert!(a.thumb().is_none());
        }
        // One within it keeps its valid dimension and loses the other.
        let mut a = attachment();
        a.try_set_thumb(
            &[4; 32],
            &[5; 32],
            &[6; 12],
            262_144,
            None,
            Some(4_096),
            Some(4_097),
        )
        .unwrap();
        let thumb = a.thumb().unwrap();
        assert_eq!((thumb.width(), thumb.height()), (Some(4_096), None));
        assert_eq!(thumb.mime(), None);

        // So a row built here encodes to what decoding would have kept.
        let mut message = plain_message(1, 1, "");
        message.inner.attachments = vec![a.inner];
        let bytes = try_encode_history_chunk(vec![message.clone()]).unwrap();
        assert_eq!(try_decode_history_chunk(&bytes).unwrap(), vec![message]);
    }

    #[test]
    fn a_forward_origin_is_kept_only_as_a_whole() {
        let mut m = plain_message(1, 1, "");
        m.set_forward("Bob", Some(vec![1; 8]), None);
        assert!(m.forwarded());
        assert_eq!(m.forward_author().as_deref(), Some("Bob"));
        assert_eq!(m.forward_origin_group_id(), None);
        assert_eq!(m.forward_origin_message_id(), None);
        m.set_forward("", None, Some(4));
        assert_eq!(m.forward_origin_message_id(), None);
        m.set_forward("", Some(vec![1; 8]), Some(4));
        assert_eq!(m.forward_origin_group_id(), Some(vec![1; 8]));
        assert_eq!(m.forward_origin_message_id(), Some(4));
    }

    #[test]
    fn constants_are_the_core_values() {
        assert_eq!(vault_max_chunk_messages(), 512);
        assert_eq!(vault_max_tail_messages(), 512);
        assert_eq!(vault_max_reentry_epochs(), 2);
        assert_eq!(vault_max_people(), 4096);
        assert_eq!(vault_max_group_info_bytes(), 1 << 20);
        assert_eq!(storage_proof_window_ms(), 120_000);
        // 1 KiB bucket plus the format byte, the nonce and the tag.
        assert_eq!(vault_sealed_len(0), Some(1024 + 29));
        assert_eq!(vault_sealed_len(1020), Some(1024 + 29));
        assert_eq!(vault_sealed_len(1021), Some(2048 + 29));
        assert_eq!(vault_sealed_len(u64::MAX), None);
    }

    #[test]
    fn debug_output_redacts_keys_and_the_registrar_seed() {
        let shown = format!("{:?}", head());
        assert!(shown.contains("<redacted>"));
        // The registrar seed (0x5E x 32 = 94 decimal) and the attachment
        // keys (2 and 5) are never printed as byte lists.
        assert!(!shown.contains("94, 94"));
        assert!(!shown.contains("2, 2, 2"));
        assert!(!shown.contains("5, 5, 5"));
    }
}
