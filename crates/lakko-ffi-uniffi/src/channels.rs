// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko-channels` (Phase 4 G4-2-crypto).
//!
//! Mirror of the wasm-bindgen surface in
//! [`crates/lakko-ffi-wasm/src/channels.rs`](../../../lakko-ffi-wasm/src/channels.rs)
//! — same six entry points, Swift / Kotlin idioms, FfiError
//! integration.
//!
//! ## API shape (Swift)
//!
//! ```swift
//! import LakkoCore
//!
//! let seed   = newChannelSeedForTest()              // 32 bytes
//! let signer = ChannelSigningKey.generate()         // fresh Ed25519
//! let wire   = try composeChannelPost(
//!     seed: seed,
//!     channelId: channelIdBytes,                    // 16 bytes
//!     epochId: 1,
//!     signingKey: signer,
//!     messageCounter: 0,
//!     plaintext: plaintext,
//! )
//! // hand `wire` to services/channels — opaque transit.
//!
//! // … on receive …
//! let post = try decodeChannelPost(wire: wire)
//! let restored = try verifyAndDecryptChannelPost(
//!     seed: seed,
//!     channelId: channelIdBytes,
//!     post: post,
//! )
//! ```
//!
//! ## What's exposed (mirrors the wasm side)
//!
//! - [`ChannelPost`] opaque handle with getters (epochId,
//!   senderPubkey, messageCounter, ciphertext, signature,
//!   toBytes).
//! - [`ChannelSigningKey`] opaque handle —
//!   `generate()` / `new(seed)` / `toSeed()` /
//!   `verifyingKey()`.
//! - `composeChannelPost` / `decodeChannelPost` /
//!   `verifyAndDecryptChannelPost` free functions.
//! - `newChannelSeedForTest` free function — single-
//!   publisher demo seed source. Production seeds come
//!   from PublisherGroup `mls_exporter(...)` once the
//!   MLS-per-channel keying lands.
//!
//! ## Subscriber-side (G4-3c)
//!
//! The same surface also exposes the HPKE seed-envelope
//! path described in [ADR-0010 §"Tier 2 — Sender-key
//! broadcast"][adr10] and pinned in audit-pack invariant
//! C-41:
//!
//! ```swift
//! let kp        = ChannelSubscriberKeypair.generate()
//! let envelope  = try sealSubscriberSeedEnvelope(
//!     subscriberPub: kp.public(),
//!     channelId:     channelIdBytes,
//!     epochId:       1,
//!     seed:          seed,            // publisher's content-key seed
//! )
//! // owner-side: POST envelope to /v1/channels/:id/subscribers/:pk/envelope
//!
//! // subscriber-side: GET envelope from same endpoint
//! let seed = try openSubscriberSeedEnvelope(
//!     keypair:   kp,
//!     channelId: channelIdBytes,
//!     epochId:   1,
//!     wire:      envelope,
//! )
//! // now decode + verifyAndDecrypt as usual
//! ```
//!
//! [adr10]: ../../../docs/adr/0010-channels-two-tier-broadcast.md

use std::sync::Mutex;

use lakko_channels::{
    compose_post, decode_post, open_seed, post::ChannelPost as InnerPost, seal_seed,
    verify_and_decrypt, ContentKeySeed, EpochId, CHANNEL_ID_LEN, SEED_ENVELOPE_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::signing::SigningKey;

use crate::FfiError;

const SIGNING_KEY_SEED_LEN: usize = 32;
const CONTENT_KEY_SEED_LEN: usize = 32;

impl From<lakko_channels::ChannelError> for FfiError {
    fn from(e: lakko_channels::ChannelError) -> Self {
        Self::Channels(e.to_string())
    }
}

// ---------------------------------------------------------
// ChannelPost — opaque handle
// ---------------------------------------------------------

/// Decoded ChannelPost. Opaque to Swift / Kotlin —
/// callers ride the getters below + the `toBytes()`
/// re-encoder. The inner mutex is there only to satisfy
/// UniFFI's `Send + Sync` requirement on `Object` types;
/// the `InnerPost` itself is `Clone + Send + Sync` so
/// the lock is uncontended (every method clones out).
#[derive(uniffi::Object)]
pub struct ChannelPost {
    inner: Mutex<InnerPost>,
}

#[uniffi::export]
impl ChannelPost {
    /// `u32` epoch id the post was encrypted under.
    #[uniffi::method]
    #[must_use]
    pub fn epoch_id(&self) -> u32 {
        self.inner
            .lock()
            .expect("channel-post lock")
            .epoch_id
            .as_u32()
    }

    /// 32-byte Ed25519 sender public key.
    #[uniffi::method]
    #[must_use]
    pub fn sender_pubkey(&self) -> Vec<u8> {
        self.inner
            .lock()
            .expect("channel-post lock")
            .sender_pubkey
            .to_vec()
    }

    /// Monotone per-(channel, epoch, sender) counter.
    #[uniffi::method]
    #[must_use]
    pub fn message_counter(&self) -> u64 {
        self.inner
            .lock()
            .expect("channel-post lock")
            .message_counter
    }

    /// Opaque ciphertext bytes (includes the 16-byte AEAD tag).
    #[uniffi::method]
    #[must_use]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.inner
            .lock()
            .expect("channel-post lock")
            .ciphertext
            .clone()
    }

    /// 64-byte Ed25519 signature.
    #[uniffi::method]
    #[must_use]
    pub fn signature(&self) -> Vec<u8> {
        self.inner
            .lock()
            .expect("channel-post lock")
            .signature
            .to_vec()
    }

    /// Re-encode to the canonical wire bytes. Round-trip
    /// of `decode → toBytes` is byte-identical (asserted
    /// by `lakko-channels`'s own `wire_roundtrip` test).
    #[uniffi::method]
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.inner.lock().expect("channel-post lock").to_bytes()
    }
}

// ---------------------------------------------------------
// ChannelSigningKey — opaque handle
// ---------------------------------------------------------

/// Ed25519 publisher signing key. Holds the 32-byte seed
/// the underlying [`SigningKey`] uses; persist + restore
/// through `toSeed()` / `new(seed)`. Deliberately not
/// `Debug` — a Debug impl would leak the seed in panic
/// messages.
#[derive(uniffi::Object)]
pub struct ChannelSigningKey {
    inner: Mutex<SigningKey>,
}

#[uniffi::export]
impl ChannelSigningKey {
    /// Fresh random key. Backed by `getrandom` (system
    /// CSPRNG on both iOS and Android).
    #[uniffi::constructor]
    #[must_use]
    pub fn generate() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            inner: Mutex::new(SigningKey::generate()),
        })
    }

    /// Reconstruct from a 32-byte seed.
    #[uniffi::constructor]
    pub fn from_seed(seed: Vec<u8>) -> Result<std::sync::Arc<Self>, FfiError> {
        let arr: [u8; SIGNING_KEY_SEED_LEN] = seed
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::Channels("signing-key seed must be exactly 32 bytes".into()))?;
        Ok(std::sync::Arc::new(Self {
            inner: Mutex::new(SigningKey::from_seed(&arr)),
        }))
    }

    /// Export the 32-byte seed for persistence. Caller-
    /// side responsibility to keep this in a secure
    /// store (Keychain / Keystore).
    #[uniffi::method]
    #[must_use]
    pub fn to_seed(&self) -> Vec<u8> {
        self.inner
            .lock()
            .expect("signing-key lock")
            .to_seed()
            .to_vec()
    }

    /// 32-byte Ed25519 verifying key — the
    /// `sender_pubkey` field of every post this key
    /// composes.
    #[uniffi::method]
    #[must_use]
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner
            .lock()
            .expect("signing-key lock")
            .verifying_key()
            .to_bytes()
            .to_vec()
    }
}

// ---------------------------------------------------------
// Free functions
// ---------------------------------------------------------

/// Compose a signed encrypted ChannelPost. Returns the
/// wire bytes ready to `POST /v1/channels/:id/posts`.
///
/// # Errors
///
/// - [`FfiError::Channels`] on wrong-length `channelId`,
///   wrong-length `seed`, oversize plaintext, or AEAD
///   seal failure.
#[uniffi::export]
pub fn compose_channel_post(
    seed: Vec<u8>,
    channel_id: Vec<u8>,
    epoch_id: u32,
    signing_key: std::sync::Arc<ChannelSigningKey>,
    message_counter: u64,
    plaintext: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel = channel_id_from_vec(channel_id)?;
    let seed_arr: [u8; CONTENT_KEY_SEED_LEN] = seed
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Channels("content-key seed must be exactly 32 bytes".into()))?;
    let seed_handle = ContentKeySeed::from_bytes_for_test(seed_arr);
    let signer = signing_key
        .inner
        .lock()
        .map_err(|_| FfiError::LockPoisoned)?;
    let post = compose_post(
        &seed_handle,
        &channel,
        EpochId::new(epoch_id),
        message_counter,
        &signer,
        &plaintext,
    )?;
    Ok(post.to_bytes())
}

/// Decode a `ChannelPost` from its on-wire bytes without
/// verifying the signature or attempting to decrypt.
///
/// # Errors
///
/// [`FfiError::Channels`] on any wire-format failure.
#[uniffi::export]
pub fn decode_channel_post(wire: Vec<u8>) -> Result<std::sync::Arc<ChannelPost>, FfiError> {
    let inner = decode_post(&wire)?;
    Ok(std::sync::Arc::new(ChannelPost {
        inner: Mutex::new(inner),
    }))
}

/// Verify the post's Ed25519 signature + AEAD-open the
/// ciphertext. Returns the recovered plaintext bytes.
///
/// # Errors
///
/// [`FfiError::Channels`] on:
/// - BadSenderKey — sender pubkey is not a valid Ed25519 point
/// - BadSignature — signature does not verify
/// - AeadOpen — wrong seed, wrong channel id, tampered
///   ciphertext
#[uniffi::export]
pub fn verify_and_decrypt_channel_post(
    seed: Vec<u8>,
    channel_id: Vec<u8>,
    post: std::sync::Arc<ChannelPost>,
) -> Result<Vec<u8>, FfiError> {
    let channel = channel_id_from_vec(channel_id)?;
    let seed_arr: [u8; CONTENT_KEY_SEED_LEN] = seed
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Channels("content-key seed must be exactly 32 bytes".into()))?;
    let seed_handle = ContentKeySeed::from_bytes_for_test(seed_arr);
    let guard = post.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
    Ok(verify_and_decrypt(&seed_handle, &channel, &guard)?)
}

/// Generate a fresh 32-byte content-key seed.
///
/// **Demo only.** Production seeds come out of the
/// PublisherGroup MLS exporter; this is the single-
/// publisher affordance until the MLS-per-channel
/// keying lands. The `for_test` suffix in the Rust name
/// keeps "non-production seed source" greppable.
#[uniffi::export]
#[must_use]
pub fn new_channel_seed_for_test() -> Vec<u8> {
    use lakko_crypto::random;
    let mut out = [0u8; CONTENT_KEY_SEED_LEN];
    random::fill(&mut out);
    out.to_vec()
}

// =========================================================
// Subscriber-side surface (G4-3c) — HPKE seed envelope
// =========================================================

const X25519_KEY_LEN: usize = 32;

/// X25519 keypair the subscriber holds locally — the public
/// half is what gets registered on the
/// `POST /v1/channels/:id/subscribers` endpoint; the secret
/// half unwraps the per-subscriber seed envelopes a
/// publisher fans out at every rotation.
///
/// Like [`ChannelSigningKey`], deliberately not `Debug` —
/// a Debug impl would leak the 32-byte secret in panic
/// messages.
#[derive(uniffi::Object)]
pub struct ChannelSubscriberKeypair {
    inner: Mutex<X25519SecretKey>,
}

#[uniffi::export]
impl ChannelSubscriberKeypair {
    /// Fresh random keypair. Backed by `getrandom` (system
    /// CSPRNG on both iOS and Android).
    #[uniffi::constructor]
    #[must_use]
    pub fn generate() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            inner: Mutex::new(X25519SecretKey::generate()),
        })
    }

    /// Reconstruct from a 32-byte secret seed.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] if `secret.len() != 32`.
    #[uniffi::constructor]
    pub fn from_secret(secret: Vec<u8>) -> Result<std::sync::Arc<Self>, FfiError> {
        let arr: [u8; X25519_KEY_LEN] = secret
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::Channels("subscriber secret must be exactly 32 bytes".into()))?;
        Ok(std::sync::Arc::new(Self {
            inner: Mutex::new(X25519SecretKey::from_bytes(arr)),
        }))
    }

    /// Export the 32-byte secret for persistence. Caller-
    /// side responsibility to keep this in a secure store
    /// (Keychain / Keystore).
    #[uniffi::method]
    #[must_use]
    pub fn secret(&self) -> Vec<u8> {
        self.inner
            .lock()
            .expect("subscriber-keypair lock")
            .to_bytes()
            .to_vec()
    }

    /// 32-byte X25519 public key — the value the
    /// subscriber registers on the channel.
    #[uniffi::method]
    #[must_use]
    pub fn public(&self) -> Vec<u8> {
        self.inner
            .lock()
            .expect("subscriber-keypair lock")
            .public_key()
            .to_bytes()
            .to_vec()
    }
}

/// Seal a 32-byte content-key seed against a subscriber's
/// X25519 public key. Returns the wire bytes ready to
/// `POST /v1/channels/:id/subscribers/:pk/envelope`.
///
/// Always returns exactly [`channel_seed_envelope_len`]
/// bytes (`eph_pub(32) || sealed_seed(32) || tag(16) = 80`).
///
/// # Errors
///
/// [`FfiError::Channels`] on:
/// - wrong-length `subscriber_pub` / `channel_id` / `seed`
/// - `subscriber_pub` not parseable as an X25519 point
/// - AEAD seal failure (impossible with ChaCha20-Poly1305
///   + valid lengths; surfaced for completeness)
#[uniffi::export]
pub fn seal_subscriber_seed_envelope(
    subscriber_pub: Vec<u8>,
    channel_id: Vec<u8>,
    epoch_id: u32,
    seed: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel = channel_id_from_vec(channel_id)?;
    let pub_arr: [u8; X25519_KEY_LEN] = subscriber_pub
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Channels("subscriber public must be exactly 32 bytes".into()))?;
    let pubkey = X25519PublicKey::from_bytes(&pub_arr)
        .map_err(|_| FfiError::Channels("subscriber public did not parse as X25519".into()))?;
    let seed_arr: [u8; CONTENT_KEY_SEED_LEN] = seed
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Channels("content-key seed must be exactly 32 bytes".into()))?;
    let seed_handle = ContentKeySeed::from_bytes_for_test(seed_arr);
    Ok(seal_seed(
        &pubkey,
        &channel,
        EpochId::new(epoch_id),
        &seed_handle,
    )?)
}

/// Open a seed envelope with the subscriber's keypair.
/// Returns the recovered 32-byte content-key seed — hand
/// it to [`verify_and_decrypt_channel_post`] to decode
/// posts on this channel for this epoch.
///
/// # Errors
///
/// [`FfiError::Channels`] on:
/// - wire shorter than `EPH_PUB_LEN + TAG_LEN`
/// - tampered ciphertext, wrong recipient, wrong
///   `(channel_id, epoch_id)` binding
/// - decrypted plaintext not exactly 32 bytes (would only
///   happen on a future schema bump that forgot to bump the
///   envelope label).
#[uniffi::export]
pub fn open_subscriber_seed_envelope(
    keypair: std::sync::Arc<ChannelSubscriberKeypair>,
    channel_id: Vec<u8>,
    epoch_id: u32,
    wire: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel = channel_id_from_vec(channel_id)?;
    let priv_guard = keypair.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
    let seed = open_seed(&priv_guard, &channel, EpochId::new(epoch_id), &wire)?;
    Ok(seed.copy_to_bytes_for_export().to_vec())
}

/// Total wire length of a seed envelope —
/// `eph_pub(32) + sealed_seed(32) + tag(16) = 80`. Exposed
/// so callers can pre-size buffers / sanity-check incoming
/// envelope blobs on the subscriber side.
#[uniffi::export]
#[must_use]
pub fn channel_seed_envelope_len() -> u32 {
    u32::try_from(SEED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

// ---------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------

fn channel_id_from_vec(slice: Vec<u8>) -> Result<[u8; CHANNEL_ID_LEN], FfiError> {
    slice
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Channels("channel_id must be exactly 16 bytes".into()))
}

// ---------------------------------------------------------
// Tests
// ---------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_key_round_trip_via_seed() {
        let a = ChannelSigningKey::generate();
        let seed = a.to_seed();
        let b = ChannelSigningKey::from_seed(seed).unwrap();
        assert_eq!(a.verifying_key(), b.verifying_key());
    }

    #[test]
    fn signing_key_from_short_seed_errors() {
        // `ChannelSigningKey` is deliberately not Debug (Debug
        // impl would leak the 32-byte seed via panic messages),
        // so `unwrap_err()` is not available — hand-rolled
        // match.
        match ChannelSigningKey::from_seed(vec![0u8; 16]) {
            Ok(_) => panic!("expected error on short seed"),
            Err(FfiError::Channels(_)) => {},
            Err(other) => panic!("expected Channels variant, got {other:?}"),
        }
    }

    #[test]
    fn compose_decode_verify_round_trip() {
        let signer = ChannelSigningKey::generate();
        let seed = new_channel_seed_for_test();
        let channel_id = vec![0x42u8; CHANNEL_ID_LEN];

        let wire = compose_channel_post(
            seed.clone(),
            channel_id.clone(),
            7,
            signer.clone(),
            42,
            b"hello channel from uniffi".to_vec(),
        )
        .unwrap();

        let post = decode_channel_post(wire).unwrap();
        assert_eq!(post.epoch_id(), 7);
        assert_eq!(post.message_counter(), 42);
        assert_eq!(post.sender_pubkey(), signer.verifying_key());

        let pt = verify_and_decrypt_channel_post(seed, channel_id, post).unwrap();
        assert_eq!(pt, b"hello channel from uniffi");
    }

    #[test]
    fn wrong_seed_fails_decrypt() {
        let signer = ChannelSigningKey::generate();
        let seed = new_channel_seed_for_test();
        let other = new_channel_seed_for_test();
        let channel_id = vec![0x77u8; CHANNEL_ID_LEN];

        let wire = compose_channel_post(seed, channel_id.clone(), 1, signer, 0, b"sealed".to_vec())
            .unwrap();
        let post = decode_channel_post(wire).unwrap();
        let err = verify_and_decrypt_channel_post(other, channel_id, post).unwrap_err();
        assert!(matches!(err, FfiError::Channels(_)));
    }

    #[test]
    fn wrong_channel_id_length_rejected() {
        let signer = ChannelSigningKey::generate();
        let seed = new_channel_seed_for_test();
        let err = compose_channel_post(
            seed,
            vec![0u8; 8], // too short
            1,
            signer,
            0,
            b"x".to_vec(),
        )
        .unwrap_err();
        assert!(matches!(err, FfiError::Channels(_)));
    }

    #[test]
    fn new_channel_seed_for_test_returns_32_bytes() {
        let seed = new_channel_seed_for_test();
        assert_eq!(seed.len(), CONTENT_KEY_SEED_LEN);
        // Sanity: two consecutive calls should not collide
        // (probabilistic — collision is 2^−256 likely).
        let other = new_channel_seed_for_test();
        assert_ne!(seed, other);
    }

    // -----------------------------------------------------
    // Subscriber-side (G4-3c) tests
    // -----------------------------------------------------

    #[test]
    fn subscriber_keypair_secret_round_trip() {
        let a = ChannelSubscriberKeypair::generate();
        let secret = a.secret();
        let b = ChannelSubscriberKeypair::from_secret(secret).unwrap();
        assert_eq!(a.public(), b.public());
    }

    #[test]
    fn subscriber_keypair_from_short_secret_errors() {
        // Deliberately not Debug — `unwrap_err()` not available.
        match ChannelSubscriberKeypair::from_secret(vec![0u8; 16]) {
            Ok(_) => panic!("expected error on short secret"),
            Err(FfiError::Channels(_)) => {},
            Err(other) => panic!("expected Channels variant, got {other:?}"),
        }
    }

    #[test]
    fn subscriber_public_is_32_bytes() {
        let kp = ChannelSubscriberKeypair::generate();
        assert_eq!(kp.public().len(), X25519_KEY_LEN);
    }

    #[test]
    fn seed_envelope_seal_open_round_trip() {
        let kp = ChannelSubscriberKeypair::generate();
        let seed = new_channel_seed_for_test();
        let channel_id = vec![0x99u8; CHANNEL_ID_LEN];

        let env = seal_subscriber_seed_envelope(kp.public(), channel_id.clone(), 5, seed.clone())
            .unwrap();
        assert_eq!(
            u32::try_from(env.len()).unwrap(),
            channel_seed_envelope_len()
        );

        let recovered = open_subscriber_seed_envelope(kp, channel_id, 5, env).unwrap();
        assert_eq!(recovered, seed);
    }

    #[test]
    fn seed_envelope_wrong_recipient_fails() {
        let kp = ChannelSubscriberKeypair::generate();
        let other = ChannelSubscriberKeypair::generate();
        let seed = new_channel_seed_for_test();
        let channel_id = vec![0x11u8; CHANNEL_ID_LEN];

        let env = seal_subscriber_seed_envelope(kp.public(), channel_id.clone(), 1, seed).unwrap();
        let err = open_subscriber_seed_envelope(other, channel_id, 1, env).unwrap_err();
        assert!(matches!(err, FfiError::Channels(_)));
    }

    #[test]
    fn seed_envelope_wrong_epoch_fails() {
        let kp = ChannelSubscriberKeypair::generate();
        let seed = new_channel_seed_for_test();
        let channel_id = vec![0x22u8; CHANNEL_ID_LEN];

        let env = seal_subscriber_seed_envelope(kp.public(), channel_id.clone(), 7, seed).unwrap();
        let err = open_subscriber_seed_envelope(kp, channel_id, 8, env).unwrap_err();
        assert!(matches!(err, FfiError::Channels(_)));
    }

    #[test]
    fn seed_envelope_short_subscriber_pub_rejected() {
        let seed = new_channel_seed_for_test();
        let err = seal_subscriber_seed_envelope(
            vec![0u8; 16], // too short
            vec![0u8; CHANNEL_ID_LEN],
            1,
            seed,
        )
        .unwrap_err();
        assert!(matches!(err, FfiError::Channels(_)));
    }

    #[test]
    fn full_subscriber_e2e_publisher_to_subscriber() {
        // The full Phase-4b receive pipeline: publisher
        // composes a post, owner seals the seed against
        // the subscriber pubkey, subscriber unwraps the
        // seed + decodes the post.
        let signer = ChannelSigningKey::generate();
        let seed = new_channel_seed_for_test();
        let channel_id = vec![0xABu8; CHANNEL_ID_LEN];

        // Publisher path: compose the post.
        let wire = compose_channel_post(
            seed.clone(),
            channel_id.clone(),
            3,
            signer.clone(),
            0,
            b"phase-4b end-to-end".to_vec(),
        )
        .unwrap();

        // Subscriber registers a keypair; publisher seals
        // the seed against it.
        let subscriber = ChannelSubscriberKeypair::generate();
        let envelope =
            seal_subscriber_seed_envelope(subscriber.public(), channel_id.clone(), 3, seed)
                .unwrap();

        // Subscriber opens the envelope and decodes the post.
        let recovered_seed =
            open_subscriber_seed_envelope(subscriber, channel_id.clone(), 3, envelope).unwrap();

        let post = decode_channel_post(wire).unwrap();
        let plaintext = verify_and_decrypt_channel_post(recovered_seed, channel_id, post).unwrap();
        assert_eq!(plaintext, b"phase-4b end-to-end");
    }
}
