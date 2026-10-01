// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of the channel directory core ([ADR-0059] S1): the
//! channel owner key and its request proof, the public listing codec, the
//! fixed topic list and the language rule.
//!
//! The shared-channels group envelope (ADR-0059 §6) lives in
//! [`crate::contacts`] beside its sibling service envelopes; the metadata
//! record v2 (topic, language, listed) is [`crate::channels::FfiChannelMeta`].
//!
//! [ADR-0059]: ../../../docs/adr/0059-channel-discover.md

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lakko_channels::copublisher::{
    compose_copublisher_binding, decode_copublisher_binding,
    ChannelCoPublisherSigner as InnerCoPublisher, CoPublisherBindingError,
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

use crate::{FfiError, Identity};

impl From<ChannelListingError> for FfiError {
    fn from(e: ChannelListingError) -> Self {
        Self::ChannelListing(format!("{}: {e}", e.code()))
    }
}

fn channel_id_from(bytes: &[u8]) -> Result<[u8; CHANNEL_ID_LEN], FfiError> {
    bytes.try_into().map_err(|_| {
        FfiError::Channels(format!(
            "channel_id must be {CHANNEL_ID_LEN} bytes, got {}",
            bytes.len()
        ))
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fresh_nonce() -> [u8; OWNER_PROOF_NONCE_LEN] {
    let mut nonce = [0u8; OWNER_PROOF_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    nonce
}

// ---------------------------------------------------------
// Owner key + request proof (ADR-0059 §1)
// ---------------------------------------------------------

/// The owner signer of one channel: the key
/// `BLAKE3-derive-key("lakko/channels/owner/v1", account_seed ‖ channel_id)`.
/// Only the account that created the channel can derive it.
#[derive(uniffi::Object)]
pub struct ChannelOwnerSigner {
    inner: Inner,
}

#[uniffi::export]
impl ChannelOwnerSigner {
    /// The 32-byte Ed25519 public key `POST /v1/channels` registers as
    /// `owner_pubkey`.
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-channel-owner-proof` header value for one request.
    /// `path` is the service path without the query
    /// (`/v1/channels/<hex>/listing`), `query` the raw query string without
    /// `?`, `body` the exact bytes sent. Every call mints a fresh nonce; a
    /// value is good for one request within the server's two-minute window.
    pub fn sign(&self, method: String, path: String, query: String, body: Vec<u8>) -> String {
        let req = OwnerRequest {
            method: &method,
            canonical_path: &path,
            query: &query,
            body: &body,
        };
        self.inner.sign_at(&req, now_ms(), fresh_nonce()).encode()
    }
}

/// The owner signer of `channel_id` for `account_identity` (the account
/// identity, never a device identity: a second device of the same account
/// derives the same key).
///
/// # Errors
///
/// [`FfiError::Channels`] when `channel_id` is not 16 bytes.
#[uniffi::export]
pub fn channel_owner_signer(
    account_identity: &Identity,
    channel_id: Vec<u8>,
) -> Result<Arc<ChannelOwnerSigner>, FfiError> {
    let channel_id = channel_id_from(&channel_id)?;
    Ok(Arc::new(ChannelOwnerSigner {
        inner: Inner::from_account_seed(&account_identity.inner.ed25519_seed_bytes(), &channel_id),
    }))
}

/// The request header an owner proof travels in.
#[uniffi::export]
#[must_use]
pub fn channel_owner_proof_header_name() -> String {
    OWNER_PROOF_HEADER_NAME.to_string()
}

// ---------------------------------------------------------
// Co-publisher keys (ADR-0059 §1, decided 2026-09-29)
// ---------------------------------------------------------

/// The frozen code of a binding refusal.
pub(crate) const fn copublisher_binding_code(e: CoPublisherBindingError) -> &'static str {
    match e {
        CoPublisherBindingError::WrongDomain => "wrongDomain",
        CoPublisherBindingError::BadLength => "badLength",
        CoPublisherBindingError::UnknownVersion => "unknownVersion",
        CoPublisherBindingError::BadSignature => "badSignature",
    }
}

impl From<CoPublisherBindingError> for FfiError {
    fn from(e: CoPublisherBindingError) -> Self {
        Self::ChannelCoPublisherBinding(format!("{}: {e}", copublisher_binding_code(e)))
    }
}

/// A co-publisher's own key for one channel: the key
/// `BLAKE3-derive-key("lakko/channels/copublisher/v1", account_seed ‖ channel_id)`,
/// which the channel's owner appoints on `services/channels`.
#[derive(uniffi::Object)]
pub struct ChannelCoPublisherSigner {
    inner: InnerCoPublisher,
}

#[uniffi::export]
impl ChannelCoPublisherSigner {
    /// The 32-byte Ed25519 public key the owner appoints.
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-channel-copublisher-proof` header value for one request
    /// (same arguments as `ChannelOwnerSigner::sign`). Accepted only on the
    /// routes a rotation fanout needs, and only once the owner appointed the
    /// key.
    pub fn sign(&self, method: String, path: String, query: String, body: Vec<u8>) -> String {
        let req = OwnerRequest {
            method: &method,
            canonical_path: &path,
            query: &query,
            body: &body,
        };
        self.inner.sign_at(&req, now_ms(), fresh_nonce()).encode()
    }
}

/// The co-publisher signer of `channel_id` for `account_identity` (the
/// account identity, the one the PublisherGroup leaf is).
///
/// # Errors
///
/// [`FfiError::Channels`] when `channel_id` is not 16 bytes.
#[uniffi::export]
pub fn channel_copublisher_signer(
    account_identity: &Identity,
    channel_id: Vec<u8>,
) -> Result<Arc<ChannelCoPublisherSigner>, FfiError> {
    let channel_id = channel_id_from(&channel_id)?;
    Ok(Arc::new(ChannelCoPublisherSigner {
        inner: InnerCoPublisher::from_account_seed(
            &account_identity.inner.ed25519_seed_bytes(),
            &channel_id,
        ),
    }))
}

/// The request header a co-publisher proof travels in.
#[uniffi::export]
#[must_use]
pub fn channel_copublisher_proof_header_name() -> String {
    COPUBLISHER_PROOF_HEADER_NAME.to_string()
}

/// How many co-publisher keys one channel may have appointed.
#[uniffi::export]
#[must_use]
pub fn channel_max_copublishers() -> u32 {
    u32::try_from(MAX_COPUBLISHERS).unwrap_or(u32::MAX)
}

/// The service path the owner signs to appoint (`PUT`) or revoke
/// (`DELETE`) `copublisher_key` for `channel_id`:
/// `/v1/channels/<channel hex>/copublishers/<key hex>`.
///
/// # Errors
///
/// [`FfiError::Channels`] when `channel_id` is not 16 bytes or the key not
/// 32.
#[uniffi::export]
pub fn channel_copublisher_path(
    channel_id: Vec<u8>,
    copublisher_key: Vec<u8>,
) -> Result<String, FfiError> {
    let channel_id = channel_id_from(&channel_id)?;
    if copublisher_key.len() != 32 {
        return Err(FfiError::Channels(format!(
            "co-publisher key must be 32 bytes, got {}",
            copublisher_key.len()
        )));
    }
    Ok(format!(
        "/v1/channels/{}/copublishers/{}",
        hex_lower(&channel_id),
        hex_lower(&copublisher_key)
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
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiChannelCoPublisherBinding {
    /// The channel the key is for (16 bytes).
    pub channel_id: Vec<u8>,
    /// The co-publisher's account identity key, which signed the binding.
    /// The owner appoints only when this equals the joiner key it approved.
    pub account_pubkey: Vec<u8>,
    /// The co-publisher key to appoint.
    pub copublisher_pubkey: Vec<u8>,
}

/// The binding a prospective co-publisher sends the owner with its knock:
/// its co-publisher key for `channel_id`, signed by `account_identity`.
///
/// # Errors
///
/// [`FfiError::Channels`] when `channel_id` is not 16 bytes.
#[uniffi::export]
pub fn compose_channel_copublisher_binding(
    account_identity: &Identity,
    channel_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel_id = channel_id_from(&channel_id)?;
    Ok(compose_copublisher_binding(
        &account_identity.inner.ed25519_seed_bytes(),
        &channel_id,
    ))
}

/// Decode and verify a binding (the signature under its embedded account
/// key). The caller still checks `account_pubkey` and `channel_id`.
///
/// # Errors
///
/// [`FfiError::ChannelCoPublisherBinding`].
#[uniffi::export]
pub fn decode_channel_copublisher_binding(
    bytes: Vec<u8>,
) -> Result<FfiChannelCoPublisherBinding, FfiError> {
    let b = decode_copublisher_binding(&bytes)?;
    Ok(FfiChannelCoPublisherBinding {
        channel_id: b.channel_id.to_vec(),
        account_pubkey: b.account_pubkey.to_vec(),
        copublisher_pubkey: b.copublisher_pubkey.to_vec(),
    })
}

// ---------------------------------------------------------
// Topics + language (ADR-0059 §5)
// ---------------------------------------------------------

/// One entry of the fixed topic list.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiChannelTopic {
    /// The stable id carried on the wire.
    pub id: u8,
    /// The English name a client localises.
    pub name: String,
}

/// The fixed topic list, in id order. Id 0 is "Other".
#[uniffi::export]
#[must_use]
pub fn channel_topics() -> Vec<FfiChannelTopic> {
    TOPICS
        .iter()
        .map(|&(id, name)| FfiChannelTopic {
            id,
            name: name.to_string(),
        })
        .collect()
}

/// The English name of topic `id`; "Other" for an id this build does not
/// know.
#[uniffi::export]
#[must_use]
pub fn channel_topic_name(id: u8) -> String {
    topic_name(id).to_string()
}

/// Whether `language` is an admissible ISO 639 code (2 or 3 ASCII lowercase
/// letters).
#[uniffi::export]
#[must_use]
pub fn channel_language_is_valid(language: String) -> bool {
    check_language(&language).is_ok()
}

// ---------------------------------------------------------
// Listing (ADR-0059 §3)
// ---------------------------------------------------------

/// One directory listing. Every field is the owner's public statement; a
/// client verifies the handle through the OPRF lookup before it shows it as
/// a link or subscribes.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiChannelListing {
    /// The channel's 16-byte id.
    pub channel_id: Vec<u8>,
    /// Canonical handle (3-32 of `[a-z0-9_]`).
    pub handle: String,
    /// The channel name (the ADR-0051 rule).
    pub name: String,
    /// The channel description (the ADR-0051 rule; may be empty).
    pub description: String,
    /// A `channelTopics()` id; unknown reads as "Other".
    pub topic: u8,
    /// ISO 639 code, required in a listing.
    pub language: String,
    /// Owner-clock UNIX milliseconds.
    pub updated_at_ms: u64,
}

impl From<ChannelListing> for FfiChannelListing {
    fn from(l: ChannelListing) -> Self {
        Self {
            channel_id: l.channel_id.to_vec(),
            handle: l.handle,
            name: l.name,
            description: l.description,
            topic: l.topic,
            language: l.language,
            updated_at_ms: l.updated_at_ms,
        }
    }
}

/// Compose a directory listing — the body `PUT /v1/channels/{id}/listing`
/// uploads.
///
/// # Errors
///
/// [`FfiError::ChannelListing`] with the frozen reason code first
/// (`handle.tooShort`, `name.empty`, `language.badLength`, ...), or
/// [`FfiError::Channels`] when `channel_id` is not 16 bytes.
#[uniffi::export]
pub fn compose_channel_listing(listing: FfiChannelListing) -> Result<Vec<u8>, FfiError> {
    let inner = ChannelListing {
        channel_id: channel_id_from(&listing.channel_id)?,
        handle: listing.handle,
        name: listing.name,
        description: listing.description,
        topic: listing.topic,
        language: listing.language,
        updated_at_ms: listing.updated_at_ms,
    };
    Ok(inner_compose(&inner)?)
}

/// Decode a directory listing. Fail-closed: any bound violation refuses the
/// whole listing.
///
/// # Errors
///
/// [`FfiError::ChannelListing`] with the frozen reason code first.
#[uniffi::export]
pub fn decode_channel_listing(bytes: Vec<u8>) -> Result<FfiChannelListing, FfiError> {
    Ok(inner_decode(&bytes)?.into())
}

/// `true` when a listing stamped `incoming_updated_at_ms` replaces the
/// stored one for the same channel. Strictly greater.
#[uniffi::export]
#[must_use]
pub fn channel_listing_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    inner_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

#[cfg(test)]
mod tests {
    #[test]
    fn copublisher_binding_round_trips_through_the_ffi() {
        let identity = crate::Identity::from_seed(vec![3; 64]).unwrap();
        let wire = compose_channel_copublisher_binding(&identity, vec![4; 16]).unwrap();
        let b = decode_channel_copublisher_binding(wire.clone()).unwrap();
        assert_eq!(b.channel_id, vec![4; 16]);
        let signer = channel_copublisher_signer(&identity, vec![4; 16]).unwrap();
        assert_eq!(b.copublisher_pubkey, signer.verifying_key());
        assert_ne!(
            signer.verifying_key(),
            channel_owner_signer(&identity, vec![4; 16])
                .unwrap()
                .verifying_key()
        );
        let mut bad = wire;
        let last = bad.len() - 1;
        bad[last] ^= 1;
        match decode_channel_copublisher_binding(bad) {
            Err(FfiError::ChannelCoPublisherBinding(m)) => assert!(m.starts_with("badSignature")),
            other => panic!("expected a binding error, got {other:?}"),
        }
        assert_eq!(
            channel_copublisher_path(vec![0xab; 16], vec![0xcd; 32]).unwrap(),
            format!(
                "/v1/channels/{}/copublishers/{}",
                "ab".repeat(16),
                "cd".repeat(32)
            )
        );
    }

    use super::*;
    use lakko_channels::owner::{verify_owner_proof, OwnerProof};

    fn listing() -> FfiChannelListing {
        FfiChannelListing {
            channel_id: vec![5; 16],
            handle: "lakko_news".to_string(),
            name: "Lakko News".to_string(),
            description: String::new(),
            topic: 1,
            language: "en".to_string(),
            updated_at_ms: 9,
        }
    }

    #[test]
    fn listing_round_trips_and_refuses_with_the_code_first() {
        let wire = compose_channel_listing(listing()).unwrap();
        assert_eq!(decode_channel_listing(wire).unwrap(), listing());
        let bad = FfiChannelListing {
            language: String::new(),
            ..listing()
        };
        match compose_channel_listing(bad).unwrap_err() {
            FfiError::ChannelListing(msg) => assert!(msg.starts_with("language.badLength: ")),
            other => panic!("expected ChannelListing, got {other:?}"),
        }
        let short_id = FfiChannelListing {
            channel_id: vec![1; 15],
            ..listing()
        };
        assert!(matches!(
            compose_channel_listing(short_id),
            Err(FfiError::Channels(_))
        ));
        assert!(channel_listing_is_newer(2, 1));
        assert!(!channel_listing_is_newer(1, 1));
    }

    #[test]
    fn topics_and_language() {
        let topics = channel_topics();
        assert_eq!(topics.len(), 24);
        assert_eq!(topics[0].name, "Other");
        assert_eq!(channel_topic_name(250), "Other");
        assert!(channel_language_is_valid("en".to_string()));
        assert!(!channel_language_is_valid("EN".to_string()));
    }

    #[test]
    fn owner_signer_signs_what_the_core_verifies() {
        let identity = Identity::from_seed(vec![3; 64]).unwrap();
        let signer = channel_owner_signer(&identity, vec![4; 16]).unwrap();
        let again = channel_owner_signer(&identity, vec![4; 16]).unwrap();
        assert_eq!(signer.verifying_key(), again.verifying_key());
        let other = channel_owner_signer(&identity, vec![5; 16]).unwrap();
        assert_ne!(signer.verifying_key(), other.verifying_key());

        let header = signer.sign(
            "PUT".to_string(),
            "/v1/channels/x/listing".to_string(),
            String::new(),
            b"body".to_vec(),
        );
        let proof = OwnerProof::parse(&header).unwrap();
        let key = lakko_crypto::signing::VerifyingKey::from_bytes(&signer.verifying_key()).unwrap();
        let req = OwnerRequest {
            method: "PUT",
            canonical_path: "/v1/channels/x/listing",
            query: "",
            body: b"body",
        };
        verify_owner_proof(&proof, &req, &key, proof.unix_ms).unwrap();
        assert!(channel_owner_signer(&identity, vec![4; 15]).is_err());
        assert_eq!(
            channel_owner_proof_header_name(),
            "x-lakko-channel-owner-proof"
        );
    }
}
