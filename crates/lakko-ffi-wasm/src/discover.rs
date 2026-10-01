// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of the channel directory core ([ADR-0059] S1), the
//! mirror of `lakko-ffi-uniffi::discover`: the channel owner key and its
//! request proof, the public listing codec, the topic list and the language
//! rule. The shared-channels group envelope lives in [`crate::contacts`];
//! channel metadata v2 in [`crate::channels`].
//!
//! [ADR-0059]: ../../../docs/adr/0059-channel-discover.md

use lakko_channels::copublisher::{
    compose_copublisher_binding, decode_copublisher_binding,
    ChannelCoPublisherSigner as InnerCoPublisher, CoPublisherBinding, CoPublisherBindingError,
    COPUBLISHER_PROOF_HEADER_NAME, MAX_COPUBLISHERS,
};
use lakko_channels::listing::{
    check_language, compose_channel_listing as inner_compose,
    decode_channel_listing as inner_decode, listing_is_newer as inner_is_newer, topic_name,
    ChannelListing, ChannelListingError, TOPICS,
};
use lakko_channels::owner::{
    ChannelOwnerSigner as Inner, OwnerRequest, OWNER_PROOF_HEADER_NAME, OWNER_PROOF_NONCE_LEN,
};
use lakko_channels::CHANNEL_ID_LEN;
use wasm_bindgen::prelude::*;

use crate::mls::Identity;

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

fn fresh_nonce() -> [u8; OWNER_PROOF_NONCE_LEN] {
    let mut nonce = [0u8; OWNER_PROOF_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    nonce
}

fn channel_id_from(bytes: &[u8]) -> Result<[u8; CHANNEL_ID_LEN], JsError> {
    bytes.try_into().map_err(|_| {
        JsError::new(&format!(
            "channel_id must be {CHANNEL_ID_LEN} bytes, got {}",
            bytes.len()
        ))
    })
}

/// The value thrown for a listing that fails a bound: an `Error` whose own
/// `channelListingError` property is the frozen
/// `ChannelListingError::code()` (`handle.tooShort`, `language.badLength`,
/// ...). The message never contains the input.
fn listing_error_value(e: ChannelListingError) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    let _ = js_sys::Reflect::set(
        err.as_ref(),
        &JsValue::from_str("channelListingError"),
        &JsValue::from_str(&e.code()),
    );
    err.into()
}

// ---------------------------------------------------------
// Owner key + request proof (ADR-0059 §1)
// ---------------------------------------------------------

/// The owner signer of one channel. Only the account that created the
/// channel can derive it.
#[wasm_bindgen]
pub struct ChannelOwnerSigner {
    inner: Inner,
}

#[wasm_bindgen]
impl ChannelOwnerSigner {
    /// The 32-byte Ed25519 public key `POST /v1/channels` registers as
    /// `owner_pubkey`.
    #[wasm_bindgen(js_name = verifyingKey)]
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-channel-owner-proof` header value for one request. `path`
    /// is the service path without the query, `query` the raw query string
    /// without `?`, `body` the exact bytes sent. Fresh nonce per call.
    pub fn sign(&self, method: &str, path: &str, query: &str, body: &[u8]) -> String {
        let req = OwnerRequest {
            method,
            canonical_path: path,
            query,
            body,
        };
        self.inner.sign_at(&req, now_ms(), fresh_nonce()).encode()
    }
}

/// The owner signer of `channelId` for `accountIdentity` (the account
/// identity, never a device identity).
///
/// # Errors
///
/// Throws when `channelId` is not 16 bytes.
#[wasm_bindgen(js_name = channelOwnerSigner)]
pub fn channel_owner_signer(
    account_identity: &Identity,
    channel_id: &[u8],
) -> Result<ChannelOwnerSigner, JsError> {
    let channel_id = channel_id_from(channel_id)?;
    Ok(ChannelOwnerSigner {
        inner: Inner::from_account_seed(
            &account_identity.mls_identity().ed25519_seed_bytes(),
            &channel_id,
        ),
    })
}

/// The request header an owner proof travels in.
#[wasm_bindgen(js_name = channelOwnerProofHeaderName)]
#[must_use]
pub fn channel_owner_proof_header_name() -> String {
    OWNER_PROOF_HEADER_NAME.to_string()
}

// ---------------------------------------------------------
// Co-publisher keys (ADR-0059 §1, decided 2026-09-29)
// ---------------------------------------------------------

const fn copublisher_binding_code(e: CoPublisherBindingError) -> &'static str {
    match e {
        CoPublisherBindingError::WrongDomain => "wrongDomain",
        CoPublisherBindingError::BadLength => "badLength",
        CoPublisherBindingError::UnknownVersion => "unknownVersion",
        CoPublisherBindingError::BadSignature => "badSignature",
    }
}

/// The value thrown for a refused binding: an `Error` whose own
/// `channelCoPublisherBindingError` property is the code (`wrongDomain`,
/// `badLength`, `unknownVersion`, `badSignature`).
fn binding_error_value(e: CoPublisherBindingError) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    let _ = js_sys::Reflect::set(
        err.as_ref(),
        &JsValue::from_str("channelCoPublisherBindingError"),
        &JsValue::from_str(copublisher_binding_code(e)),
    );
    err.into()
}

/// A co-publisher's own key for one channel, which the channel's owner
/// appoints on `services/channels`.
#[wasm_bindgen]
pub struct ChannelCoPublisherSigner {
    inner: InnerCoPublisher,
}

#[wasm_bindgen]
impl ChannelCoPublisherSigner {
    /// The 32-byte Ed25519 public key the owner appoints.
    #[wasm_bindgen(js_name = verifyingKey)]
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-channel-copublisher-proof` header value for one request
    /// (same arguments as `ChannelOwnerSigner.sign`). Fresh nonce per call.
    pub fn sign(&self, method: &str, path: &str, query: &str, body: &[u8]) -> String {
        let req = OwnerRequest {
            method,
            canonical_path: path,
            query,
            body,
        };
        self.inner.sign_at(&req, now_ms(), fresh_nonce()).encode()
    }
}

/// The co-publisher signer of `channelId` for `accountIdentity`.
///
/// # Errors
///
/// Throws when `channelId` is not 16 bytes.
#[wasm_bindgen(js_name = channelCoPublisherSigner)]
pub fn channel_copublisher_signer(
    account_identity: &Identity,
    channel_id: &[u8],
) -> Result<ChannelCoPublisherSigner, JsError> {
    let channel_id = channel_id_from(channel_id)?;
    Ok(ChannelCoPublisherSigner {
        inner: InnerCoPublisher::from_account_seed(
            &account_identity.mls_identity().ed25519_seed_bytes(),
            &channel_id,
        ),
    })
}

/// The request header a co-publisher proof travels in.
#[wasm_bindgen(js_name = channelCoPublisherProofHeaderName)]
#[must_use]
pub fn channel_copublisher_proof_header_name() -> String {
    COPUBLISHER_PROOF_HEADER_NAME.to_string()
}

/// How many co-publisher keys one channel may have appointed.
#[wasm_bindgen(js_name = channelMaxCoPublishers)]
#[must_use]
pub fn channel_max_copublishers() -> u32 {
    u32::try_from(MAX_COPUBLISHERS).unwrap_or(u32::MAX)
}

/// The service path the owner signs to appoint (`PUT`) or revoke (`DELETE`)
/// `copublisherKey` for `channelId`.
///
/// # Errors
///
/// Throws when `channelId` is not 16 bytes or the key not 32.
#[wasm_bindgen(js_name = channelCoPublisherPath)]
pub fn channel_copublisher_path(
    channel_id: &[u8],
    copublisher_key: &[u8],
) -> Result<String, JsError> {
    let channel_id = channel_id_from(channel_id)?;
    if copublisher_key.len() != 32 {
        return Err(JsError::new(&format!(
            "co-publisher key must be 32 bytes, got {}",
            copublisher_key.len()
        )));
    }
    Ok(format!(
        "/v1/channels/{}/copublishers/{}",
        hex_lower(&channel_id),
        hex_lower(copublisher_key)
    ))
}

fn hex_lower(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A verified co-publisher binding.
#[wasm_bindgen]
pub struct ChannelCoPublisherBinding {
    inner: CoPublisherBinding,
}

#[wasm_bindgen]
impl ChannelCoPublisherBinding {
    /// The channel the key is for (16 bytes).
    #[wasm_bindgen(getter, js_name = channelId)]
    pub fn channel_id(&self) -> Vec<u8> {
        self.inner.channel_id.to_vec()
    }

    /// The co-publisher's account identity key, which signed the binding.
    /// Appoint only when it equals the joiner key that was approved.
    #[wasm_bindgen(getter, js_name = accountPubkey)]
    pub fn account_pubkey(&self) -> Vec<u8> {
        self.inner.account_pubkey.to_vec()
    }

    /// The co-publisher key to appoint.
    #[wasm_bindgen(getter, js_name = copublisherPubkey)]
    pub fn copublisher_pubkey(&self) -> Vec<u8> {
        self.inner.copublisher_pubkey.to_vec()
    }
}

/// The binding a prospective co-publisher sends the owner with its knock.
///
/// # Errors
///
/// Throws when `channelId` is not 16 bytes.
#[wasm_bindgen(js_name = composeChannelCoPublisherBinding)]
pub fn compose_channel_copublisher_binding(
    account_identity: &Identity,
    channel_id: &[u8],
) -> Result<Vec<u8>, JsError> {
    let channel_id = channel_id_from(channel_id)?;
    Ok(compose_copublisher_binding(
        &account_identity.mls_identity().ed25519_seed_bytes(),
        &channel_id,
    ))
}

/// Decode and verify a binding (the signature under its embedded account
/// key). The caller still checks `accountPubkey` and `channelId`.
///
/// # Errors
///
/// Throws an `Error` carrying `channelCoPublisherBindingError`.
#[wasm_bindgen(js_name = decodeChannelCoPublisherBinding)]
pub fn decode_channel_copublisher_binding(
    bytes: &[u8],
) -> Result<ChannelCoPublisherBinding, JsValue> {
    decode_copublisher_binding(bytes)
        .map(|inner| ChannelCoPublisherBinding { inner })
        .map_err(binding_error_value)
}

// ---------------------------------------------------------
// Topics + language (ADR-0059 §5)
// ---------------------------------------------------------

/// The topic ids, in order (parallel to `channelTopicNames()`).
#[wasm_bindgen(js_name = channelTopicIds)]
#[must_use]
pub fn channel_topic_ids() -> Vec<u8> {
    TOPICS.iter().map(|&(id, _)| id).collect()
}

/// The English topic names, in id order. Id 0 is "Other".
#[wasm_bindgen(js_name = channelTopicNames)]
#[must_use]
pub fn channel_topic_names() -> Vec<String> {
    TOPICS.iter().map(|&(_, name)| name.to_string()).collect()
}

/// The English name of topic `id`; "Other" for an unknown id.
#[wasm_bindgen(js_name = channelTopicName)]
#[must_use]
pub fn channel_topic_name(id: u8) -> String {
    topic_name(id).to_string()
}

/// Whether `language` is an admissible ISO 639 code (2 or 3 ASCII lowercase
/// letters).
#[wasm_bindgen(js_name = channelLanguageIsValid)]
#[must_use]
pub fn channel_language_is_valid(language: &str) -> bool {
    check_language(language).is_ok()
}

// ---------------------------------------------------------
// Listing (ADR-0059 §3)
// ---------------------------------------------------------

/// A decoded directory listing. Every field is the owner's public statement;
/// verify the handle through the OPRF lookup before showing it as a link.
#[wasm_bindgen]
pub struct ChannelListingRecord {
    inner: ChannelListing,
}

#[wasm_bindgen]
impl ChannelListingRecord {
    /// The channel's 16-byte id.
    #[wasm_bindgen(getter, js_name = channelId)]
    pub fn channel_id(&self) -> Vec<u8> {
        self.inner.channel_id.to_vec()
    }

    /// The canonical handle.
    #[wasm_bindgen(getter)]
    pub fn handle(&self) -> String {
        self.inner.handle.clone()
    }

    /// The channel name.
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> String {
        self.inner.name.clone()
    }

    /// The channel description (possibly empty).
    #[wasm_bindgen(getter)]
    pub fn description(&self) -> String {
        self.inner.description.clone()
    }

    /// The topic id; unknown reads as "Other".
    #[wasm_bindgen(getter)]
    pub fn topic(&self) -> u8 {
        self.inner.topic
    }

    /// The ISO 639 language.
    #[wasm_bindgen(getter)]
    pub fn language(&self) -> String {
        self.inner.language.clone()
    }

    /// Owner-clock UNIX milliseconds, a `bigint`.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }
}

/// Compose a directory listing — the body `PUT /v1/channels/{id}/listing`
/// uploads.
///
/// # Errors
///
/// Throws an `Error` with a `channelListingError` code, or a plain `Error`
/// when `channelId` is not 16 bytes.
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen(js_name = composeChannelListing)]
pub fn compose_channel_listing(
    channel_id: &[u8],
    handle: &str,
    name: &str,
    description: &str,
    topic: u8,
    language: &str,
    updated_at_ms: u64,
) -> Result<Vec<u8>, JsValue> {
    let channel_id = channel_id_from(channel_id).map_err(JsValue::from)?;
    let listing = ChannelListing {
        channel_id,
        handle: handle.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        topic,
        language: language.to_string(),
        updated_at_ms,
    };
    inner_compose(&listing).map_err(listing_error_value)
}

/// Decode a directory listing. Fail-closed.
///
/// # Errors
///
/// Throws an `Error` with a `channelListingError` code.
#[wasm_bindgen(js_name = decodeChannelListing)]
pub fn decode_channel_listing(bytes: &[u8]) -> Result<ChannelListingRecord, JsValue> {
    inner_decode(bytes)
        .map(|inner| ChannelListingRecord { inner })
        .map_err(listing_error_value)
}

/// `true` when a listing stamped `incomingUpdatedAtMs` replaces the stored
/// one for the same channel. Strictly greater.
#[wasm_bindgen(js_name = channelListingIsNewer)]
#[must_use]
pub fn channel_listing_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    inner_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The throwing paths build a `js_sys::Error`, which cannot be constructed
    // on a host target; only the positive paths run here, the bounds are
    // pinned by `lakko-channels`'s own tests.

    #[test]
    fn listing_round_trips_wasm_host() {
        let wire =
            compose_channel_listing(&[5; 16], "lakko_news", "Lakko News", "", 1, "en", 9).unwrap();
        let record = decode_channel_listing(&wire).unwrap();
        assert_eq!(record.channel_id(), vec![5; 16]);
        assert_eq!(record.handle(), "lakko_news");
        assert_eq!(record.topic(), 1);
        assert_eq!(record.language(), "en");
        assert_eq!(record.updated_at_ms(), 9);
        assert!(channel_listing_is_newer(10, 9));
    }

    #[test]
    fn topics_wasm_host() {
        assert_eq!(channel_topic_ids().len(), 24);
        assert_eq!(channel_topic_names()[0], "Other");
        assert_eq!(channel_topic_name(99), "Other");
        assert!(channel_language_is_valid("fil"));
        assert!(!channel_language_is_valid("F"));
    }

    #[test]
    fn owner_signer_is_per_channel_wasm_host() {
        let identity = Identity::from_seed(&[3; 64]).unwrap();
        let a = channel_owner_signer(&identity, &[1; 16]).unwrap();
        let b = channel_owner_signer(&identity, &[2; 16]).unwrap();
        assert_ne!(a.verifying_key(), b.verifying_key());
        let header = a.sign("PUT", "/v1/channels/x/listing", "", b"");
        assert!(header.starts_with("v1."));
    }

    #[test]
    fn copublisher_signer_and_binding_wasm_host() {
        let identity = Identity::from_seed(&[3; 64]).unwrap();
        let signer = channel_copublisher_signer(&identity, &[1; 16]).unwrap();
        let owner = channel_owner_signer(&identity, &[1; 16]).unwrap();
        assert_ne!(signer.verifying_key(), owner.verifying_key());
        let wire = compose_channel_copublisher_binding(&identity, &[1; 16]).unwrap();
        let b = decode_channel_copublisher_binding(&wire).unwrap();
        assert_eq!(b.copublisher_pubkey(), signer.verifying_key());
        assert_eq!(b.channel_id(), vec![1; 16]);
        let header = signer.sign("GET", "/v1/channels/x/subscribers", "", b"");
        assert!(header.starts_with("v1."));
        assert_eq!(header.split('.').count(), 5);
    }
}
