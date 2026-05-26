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
//! ## What's NOT exposed yet
//!
//! Subscriber-side HPKE seal_seed / open_seed —
//! follow-up sub-sprint when the iOS / Android subscriber
//! UI flow has a place to drive them. Wire surface ready
//! in `lakko-channels`; UniFFI binding is mechanical to
//! add when the call sites exist.

use std::sync::Mutex;

use lakko_channels::{
    compose_post, decode_post,
    post::ChannelPost as InnerPost, verify_and_decrypt, ContentKeySeed, EpochId,
    CHANNEL_ID_LEN,
};
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
        self.inner.lock().expect("channel-post lock").epoch_id.as_u32()
    }

    /// 32-byte Ed25519 sender public key.
    #[uniffi::method]
    #[must_use]
    pub fn sender_pubkey(&self) -> Vec<u8> {
        self.inner.lock().expect("channel-post lock").sender_pubkey.to_vec()
    }

    /// Monotone per-(channel, epoch, sender) counter.
    #[uniffi::method]
    #[must_use]
    pub fn message_counter(&self) -> u64 {
        self.inner.lock().expect("channel-post lock").message_counter
    }

    /// Opaque ciphertext bytes (includes the 16-byte AEAD tag).
    #[uniffi::method]
    #[must_use]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.inner.lock().expect("channel-post lock").ciphertext.clone()
    }

    /// 64-byte Ed25519 signature.
    #[uniffi::method]
    #[must_use]
    pub fn signature(&self) -> Vec<u8> {
        self.inner.lock().expect("channel-post lock").signature.to_vec()
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
        let arr: [u8; SIGNING_KEY_SEED_LEN] = seed.as_slice().try_into().map_err(|_| {
            FfiError::Channels("signing-key seed must be exactly 32 bytes".into())
        })?;
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
        self.inner.lock().expect("signing-key lock").to_seed().to_vec()
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
    let seed_arr: [u8; CONTENT_KEY_SEED_LEN] =
        seed.as_slice().try_into().map_err(|_| {
            FfiError::Channels("content-key seed must be exactly 32 bytes".into())
        })?;
    let seed_handle = ContentKeySeed::from_bytes_for_test(seed_arr);
    let signer = signing_key.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
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
    let seed_arr: [u8; CONTENT_KEY_SEED_LEN] =
        seed.as_slice().try_into().map_err(|_| {
            FfiError::Channels("content-key seed must be exactly 32 bytes".into())
        })?;
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

        let pt =
            verify_and_decrypt_channel_post(seed, channel_id, post).unwrap();
        assert_eq!(pt, b"hello channel from uniffi");
    }

    #[test]
    fn wrong_seed_fails_decrypt() {
        let signer = ChannelSigningKey::generate();
        let seed = new_channel_seed_for_test();
        let other = new_channel_seed_for_test();
        let channel_id = vec![0x77u8; CHANNEL_ID_LEN];

        let wire =
            compose_channel_post(seed, channel_id.clone(), 1, signer, 0, b"sealed".to_vec())
                .unwrap();
        let post = decode_channel_post(wire).unwrap();
        let err =
            verify_and_decrypt_channel_post(other, channel_id, post).unwrap_err();
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
}
