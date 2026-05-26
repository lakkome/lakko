// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko-channels`
//! (Phase 4 G4-2-crypto).
//!
//! Closes the "stub publish" gap from the G4-2 web
//! commit — the web channels UI now composes real
//! signed encrypted ChannelPosts via this surface
//! instead of the random 64-byte stub.
//!
//! ## API shape (JS)
//!
//! ```ts
//! import {
//!   composeChannelPost,
//!   decodeChannelPost,
//!   verifyAndDecryptChannelPost,
//!   newChannelSeedForTest,
//!   newChannelSigningKey,
//! } from "@lakko/wasm";
//!
//! const seed   = newChannelSeedForTest();          // 32 bytes
//! const signer = newChannelSigningKey();           // fresh Ed25519
//! const wire   = composeChannelPost(
//!   seed, channelIdBytes, /* epoch */ 1n, signer,
//!   /* counter */ 0n, plaintextBytes,
//! );
//! // hand `wire` to services/channels — opaque transit.
//!
//! // ... on receive ...
//! const post     = decodeChannelPost(wire);
//! const restored = verifyAndDecryptChannelPost(
//!   seed, channelIdBytes, post,
//! );
//! ```
//!
//! ## Scope
//!
//! The composer + opener side of ADR-0010 Tier 2 (per-
//! message AEAD keyed off the content-key seed, Ed25519
//! signature on every post) **and** the subscriber-side
//! HPKE seed-envelope path (G4-3c):
//!
//! ```ts
//! import {
//!   ChannelSubscriberKeypair,
//!   sealSubscriberSeedEnvelope,
//!   openSubscriberSeedEnvelope,
//!   channelSeedEnvelopeLen,
//! } from "@lakko/wasm";
//!
//! const kp        = ChannelSubscriberKeypair.generate();
//! const envelope  = sealSubscriberSeedEnvelope(
//!   kp.public(), channelIdBytes, /* epoch */ 1, seed,
//! );
//! // ...transport over POST /v1/channels/:id/subscribers/:pk/envelope
//! const seed = openSubscriberSeedEnvelope(
//!   kp, channelIdBytes, 1, envelope,
//! );
//! // hand the recovered seed to verifyAndDecryptChannelPost
//! ```
//!
//! ## What `newChannelSeedForTest` is for
//!
//! Production seeds come out of the PublisherGroup's
//! MLS exporter — there is no client-side seed
//! generator on the publisher path. The
//! `*ForTest` JS export wraps
//! `ContentKeySeed::from_bytes_for_test` so a
//! single-publisher demo channel (no real subscribers
//! yet) can drive a round-trip; the surface name
//! carries the `ForTest` suffix to make the audit-
//! pack reviewer's life easier when grepping for
//! "non-MLS seed sources".

use wasm_bindgen::prelude::*;

use lakko_channels::{
    compose_post, decode_post, open_seed, post::ChannelPost as RustChannelPost, seal_seed,
    verify_and_decrypt, ContentKeySeed, EpochId, CHANNEL_ID_LEN, SEED_ENVELOPE_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::signing::SigningKey;

const SIGNING_KEY_SEED_LEN: usize = 32;
const CONTENT_KEY_SEED_LEN: usize = 32;

// ---------------------------------------------------------
// ChannelPost (opaque to JS)
// ---------------------------------------------------------

/// Decoded representation of a ChannelPost. JS treats this
/// as an opaque handle; the underlying field bytes are
/// accessible through dedicated getters so a future schema
/// bump can change the inner layout without breaking the
/// wire surface.
#[wasm_bindgen(js_name = "ChannelPost")]
pub struct WasmChannelPost {
    inner: RustChannelPost,
}

#[wasm_bindgen(js_class = "ChannelPost")]
impl WasmChannelPost {
    /// `u32` epoch id the post was encrypted under.
    #[wasm_bindgen(getter, js_name = "epochId")]
    #[must_use]
    pub fn epoch_id(&self) -> u32 {
        self.inner.epoch_id.as_u32()
    }

    /// 32-byte Ed25519 sender public key.
    #[wasm_bindgen(getter, js_name = "senderPubkey")]
    #[must_use]
    pub fn sender_pubkey(&self) -> Vec<u8> {
        self.inner.sender_pubkey.to_vec()
    }

    /// `u64` monotone per-(channel, epoch, sender) counter.
    /// Returned as a JS BigInt so values above 2^53 do not
    /// lose precision.
    #[wasm_bindgen(getter, js_name = "messageCounter")]
    #[must_use]
    pub fn message_counter(&self) -> u64 {
        self.inner.message_counter
    }

    /// Opaque ciphertext bytes (includes the 16-byte AEAD
    /// tag).
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.inner.ciphertext.clone()
    }

    /// 64-byte Ed25519 signature.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn signature(&self) -> Vec<u8> {
        self.inner.signature.to_vec()
    }

    /// Re-encode to the canonical wire bytes. Round-trip
    /// of `decode → toBytes` is byte-identical (asserted
    /// by `lakko-channels`'s own `wire_roundtrip` test).
    #[wasm_bindgen(js_name = "toBytes")]
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.inner.to_bytes()
    }
}

// ---------------------------------------------------------
// ChannelSigningKey — Ed25519 publisher identity
// ---------------------------------------------------------

/// Ed25519 signing key wrapper for the publisher side.
/// Holds the same 32-byte seed `lakko_crypto::signing::
/// SigningKey` does; exposes a deterministic
/// `verifyingKey()` getter so JS can record the publisher
/// pubkey alongside the seed in local storage.
#[wasm_bindgen(js_name = "ChannelSigningKey")]
pub struct WasmChannelSigningKey {
    inner: SigningKey,
}

#[wasm_bindgen(js_class = "ChannelSigningKey")]
impl WasmChannelSigningKey {
    /// Fresh random key. Backed by `crypto.getRandomValues`
    /// on the wasm side via `getrandom + js` feature.
    #[wasm_bindgen(js_name = "generate")]
    #[must_use]
    pub fn generate_js() -> WasmChannelSigningKey {
        WasmChannelSigningKey {
            inner: SigningKey::generate(),
        }
    }

    /// Reconstruct from a 32-byte seed (the form the
    /// caller persists in their local key store).
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8]) -> Result<WasmChannelSigningKey, JsError> {
        let arr: [u8; SIGNING_KEY_SEED_LEN] = seed
            .try_into()
            .map_err(|_| JsError::new("signing-key seed must be exactly 32 bytes"))?;
        Ok(WasmChannelSigningKey {
            inner: SigningKey::from_seed(&arr),
        })
    }

    /// Export the 32-byte seed for persistence.
    /// Caller-side responsibility to keep this in a
    /// secure store.
    #[wasm_bindgen(js_name = "toSeed")]
    #[must_use]
    pub fn to_seed(&self) -> Vec<u8> {
        self.inner.to_seed().to_vec()
    }

    /// 32-byte Ed25519 verifying key — the
    /// `sender_pubkey` field of every post this key
    /// composes.
    #[wasm_bindgen(js_name = "verifyingKey")]
    #[must_use]
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }
}

// ---------------------------------------------------------
// Free functions — compose / decode / verify
// ---------------------------------------------------------

/// Compose a signed encrypted ChannelPost.
///
/// `channelId` must be 16 bytes (ADR-0010); the JS caller
/// hands it as a `Uint8Array` — typically the decoded form
/// of the 32-char hex id from the URL. `seed` is the
/// caller-side content-key seed (in production: result of
/// `lakko_mls::group::Group::export_secret(
/// "lakko/channel/v1", channelId, 32)`).
///
/// Throws on:
/// - wrong `channelId` length
/// - wrong `seed` length
/// - `plaintext.length` exceeds `lakko_channels::post::
///   MAX_PLAINTEXT_LEN` (16 MiB)
#[wasm_bindgen(js_name = "composeChannelPost")]
pub fn compose_channel_post(
    seed: &[u8],
    channel_id: &[u8],
    epoch_id: u32,
    signing_key: &WasmChannelSigningKey,
    message_counter: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>, JsError> {
    let channel = channel_id_from_slice(channel_id)?;
    let seed_arr: [u8; CONTENT_KEY_SEED_LEN] = seed
        .try_into()
        .map_err(|_| JsError::new("content-key seed must be exactly 32 bytes"))?;
    let seed_handle = ContentKeySeed::from_bytes_for_test(seed_arr);
    let post = compose_post(
        &seed_handle,
        &channel,
        EpochId::new(epoch_id),
        message_counter,
        &signing_key.inner,
        plaintext,
    )
    .map_err(|e| JsError::new(&format!("compose_post: {e}")))?;
    Ok(post.to_bytes())
}

/// Decode a `ChannelPost` from its on-wire bytes without
/// verifying the signature or attempting to decrypt.
/// Throws on a wire-format failure.
#[wasm_bindgen(js_name = "decodeChannelPost")]
pub fn decode_channel_post(wire: &[u8]) -> Result<WasmChannelPost, JsError> {
    let inner = decode_post(wire).map_err(|e| JsError::new(&format!("decode_post: {e}")))?;
    Ok(WasmChannelPost { inner })
}

/// Verify the post's Ed25519 signature + AEAD-open the
/// ciphertext. Returns the recovered plaintext bytes.
///
/// Throws with `BadSignature` if the signature does not
/// validate, `AeadOpen` if the ciphertext is tampered or
/// the seed is wrong, `BadSenderKey` if the sender pubkey
/// does not parse as Ed25519.
#[wasm_bindgen(js_name = "verifyAndDecryptChannelPost")]
pub fn verify_and_decrypt_channel_post(
    seed: &[u8],
    channel_id: &[u8],
    post: &WasmChannelPost,
) -> Result<Vec<u8>, JsError> {
    let channel = channel_id_from_slice(channel_id)?;
    let seed_arr: [u8; CONTENT_KEY_SEED_LEN] = seed
        .try_into()
        .map_err(|_| JsError::new("content-key seed must be exactly 32 bytes"))?;
    let seed_handle = ContentKeySeed::from_bytes_for_test(seed_arr);
    verify_and_decrypt(&seed_handle, &channel, &post.inner)
        .map_err(|e| JsError::new(&format!("verify_and_decrypt: {e}")))
}

/// Generate a fresh 32-byte content-key seed.
///
/// Production seeds come out of the PublisherGroup MLS
/// exporter; this is the demo / single-publisher
/// affordance for the web channels UI before the MLS-
/// per-channel wiring lands. The `ForTest` suffix on
/// the JS name makes that scope explicit — grep the
/// codebase for "ForTest" to find non-production seed
/// sources.
#[wasm_bindgen(js_name = "newChannelSeedForTest")]
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

/// X25519 keypair the subscriber holds locally. JS treats
/// this as an opaque handle; the secret never leaves the
/// boundary unless the caller explicitly invokes
/// `secret()` to persist it in their secure-store
/// equivalent (IndexedDB-encrypted on the web).
#[wasm_bindgen(js_name = "ChannelSubscriberKeypair")]
pub struct WasmChannelSubscriberKeypair {
    inner: X25519SecretKey,
}

#[wasm_bindgen(js_class = "ChannelSubscriberKeypair")]
impl WasmChannelSubscriberKeypair {
    /// Fresh random keypair. Backed by
    /// `crypto.getRandomValues` via the `getrandom + js`
    /// feature.
    #[wasm_bindgen(js_name = "generate")]
    #[must_use]
    pub fn generate_js() -> WasmChannelSubscriberKeypair {
        WasmChannelSubscriberKeypair {
            inner: X25519SecretKey::generate(),
        }
    }

    /// Reconstruct from a 32-byte secret seed.
    #[wasm_bindgen(constructor)]
    pub fn new(secret: &[u8]) -> Result<WasmChannelSubscriberKeypair, JsError> {
        let arr: [u8; X25519_KEY_LEN] = secret
            .try_into()
            .map_err(|_| JsError::new("subscriber secret must be exactly 32 bytes"))?;
        Ok(WasmChannelSubscriberKeypair {
            inner: X25519SecretKey::from_bytes(arr),
        })
    }

    /// Export the 32-byte secret for persistence. Caller-
    /// side responsibility to keep it encrypted.
    #[wasm_bindgen(js_name = "secret")]
    #[must_use]
    pub fn secret(&self) -> Vec<u8> {
        self.inner.to_bytes().to_vec()
    }

    /// 32-byte X25519 public key — the value the
    /// subscriber registers on the channel.
    #[wasm_bindgen(js_name = "public")]
    #[must_use]
    pub fn public(&self) -> Vec<u8> {
        self.inner.public_key().to_bytes().to_vec()
    }
}

/// Seal a 32-byte content-key seed against a subscriber's
/// X25519 public key. Returns the wire bytes ready to
/// `POST /v1/channels/:id/subscribers/:pk/envelope`.
///
/// Always returns exactly [`channel_seed_envelope_len`]
/// bytes (`eph_pub(32) || sealed_seed(32) || tag(16) = 80`).
///
/// Throws on wrong-length args / non-X25519 public /
/// AEAD seal failure.
#[wasm_bindgen(js_name = "sealSubscriberSeedEnvelope")]
pub fn seal_subscriber_seed_envelope(
    subscriber_pub: &[u8],
    channel_id: &[u8],
    epoch_id: u32,
    seed: &[u8],
) -> Result<Vec<u8>, JsError> {
    let channel = channel_id_from_slice(channel_id)?;
    let pub_arr: [u8; X25519_KEY_LEN] = subscriber_pub
        .try_into()
        .map_err(|_| JsError::new("subscriber public must be exactly 32 bytes"))?;
    let pubkey = X25519PublicKey::from_bytes(&pub_arr)
        .map_err(|_| JsError::new("subscriber public did not parse as X25519"))?;
    let seed_arr: [u8; CONTENT_KEY_SEED_LEN] = seed
        .try_into()
        .map_err(|_| JsError::new("content-key seed must be exactly 32 bytes"))?;
    let seed_handle = ContentKeySeed::from_bytes_for_test(seed_arr);
    seal_seed(&pubkey, &channel, EpochId::new(epoch_id), &seed_handle)
        .map_err(|e| JsError::new(&format!("seal_seed: {e}")))
}

/// Open a seed envelope with the subscriber's keypair.
/// Returns the recovered 32-byte content-key seed — hand
/// it to `verifyAndDecryptChannelPost` to decode posts on
/// this channel for this epoch.
///
/// Throws on wire-shape / AEAD-open / wrong-recipient /
/// wrong-`(channel_id, epoch_id)` binding failures.
#[wasm_bindgen(js_name = "openSubscriberSeedEnvelope")]
pub fn open_subscriber_seed_envelope(
    keypair: &WasmChannelSubscriberKeypair,
    channel_id: &[u8],
    epoch_id: u32,
    wire: &[u8],
) -> Result<Vec<u8>, JsError> {
    let channel = channel_id_from_slice(channel_id)?;
    let seed = open_seed(&keypair.inner, &channel, EpochId::new(epoch_id), wire)
        .map_err(|e| JsError::new(&format!("open_seed: {e}")))?;
    Ok(seed.copy_to_bytes_for_export().to_vec())
}

/// Total wire length of a seed envelope —
/// `eph_pub(32) + sealed_seed(32) + tag(16) = 80`. Exposed
/// so JS can pre-size buffers / sanity-check incoming
/// envelope blobs.
#[wasm_bindgen(js_name = "channelSeedEnvelopeLen")]
#[must_use]
pub fn channel_seed_envelope_len() -> u32 {
    u32::try_from(SEED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

// ---------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------

fn channel_id_from_slice(slice: &[u8]) -> Result<[u8; CHANNEL_ID_LEN], JsError> {
    slice
        .try_into()
        .map_err(|_| JsError::new("channel_id must be exactly 16 bytes"))
}

// ---------------------------------------------------------
// Tests (host-side; the wasm-target tests live elsewhere)
// ---------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_key_round_trip_via_seed() {
        let a = WasmChannelSigningKey::generate_js();
        let seed = a.to_seed();
        let b = WasmChannelSigningKey::new(&seed).unwrap();
        assert_eq!(a.verifying_key(), b.verifying_key());
    }

    #[test]
    fn compose_then_decode_then_verify_round_trip() {
        let signer = WasmChannelSigningKey::generate_js();
        let seed = new_channel_seed_for_test();
        let channel_id = [0x42u8; CHANNEL_ID_LEN];

        let wire = compose_channel_post(
            &seed,
            &channel_id,
            7,
            &signer,
            42,
            b"hello channel from wasm",
        )
        .unwrap();

        let post = decode_channel_post(&wire).unwrap();
        assert_eq!(post.epoch_id(), 7);
        assert_eq!(post.message_counter(), 42);
        assert_eq!(post.sender_pubkey(), signer.verifying_key());

        let pt = verify_and_decrypt_channel_post(&seed, &channel_id, &post).unwrap();
        assert_eq!(pt, b"hello channel from wasm");
    }

    // Negative-path tests (wrong seed / wrong channel-id
    // length / etc.) are gated to wasm-target runs only —
    // `JsError::new` panics on host targets per
    // wasm-bindgen-0.2's documented "non-wasm targets" rule.
    // The underlying Rust-level rejections are covered by
    // `lakko-channels`'s own unit tests; this surface only
    // re-checks the positive path that the JS side cares
    // about.

    // -----------------------------------------------------
    // Subscriber-side (G4-3c) tests
    // -----------------------------------------------------

    #[test]
    fn subscriber_keypair_secret_round_trip_wasm_host() {
        let a = WasmChannelSubscriberKeypair::generate_js();
        let secret = a.secret();
        let b = WasmChannelSubscriberKeypair::new(&secret).unwrap();
        assert_eq!(a.public(), b.public());
    }

    #[test]
    fn subscriber_public_is_32_bytes_wasm_host() {
        let kp = WasmChannelSubscriberKeypair::generate_js();
        assert_eq!(kp.public().len(), X25519_KEY_LEN);
    }

    #[test]
    fn seed_envelope_seal_open_round_trip_wasm_host() {
        let kp = WasmChannelSubscriberKeypair::generate_js();
        let seed = new_channel_seed_for_test();
        let channel_id = [0x99u8; CHANNEL_ID_LEN];

        let env = seal_subscriber_seed_envelope(&kp.public(), &channel_id, 5, &seed).unwrap();
        assert_eq!(
            u32::try_from(env.len()).unwrap(),
            channel_seed_envelope_len()
        );

        let recovered = open_subscriber_seed_envelope(&kp, &channel_id, 5, &env).unwrap();
        assert_eq!(recovered, seed);
    }

    #[test]
    fn full_subscriber_e2e_wasm_host() {
        // End-to-end (publisher → seal → subscriber → open
        // → verifyAndDecrypt) on the wasm host side.
        let signer = WasmChannelSigningKey::generate_js();
        let seed = new_channel_seed_for_test();
        let channel_id = [0xCDu8; CHANNEL_ID_LEN];

        let wire =
            compose_channel_post(&seed, &channel_id, 2, &signer, 0, b"phase-4b wasm e2e").unwrap();

        let subscriber = WasmChannelSubscriberKeypair::generate_js();
        let envelope =
            seal_subscriber_seed_envelope(&subscriber.public(), &channel_id, 2, &seed).unwrap();
        let recovered_seed =
            open_subscriber_seed_envelope(&subscriber, &channel_id, 2, &envelope).unwrap();

        let post = decode_channel_post(&wire).unwrap();
        let plaintext =
            verify_and_decrypt_channel_post(&recovered_seed, &channel_id, &post).unwrap();
        assert_eq!(plaintext, b"phase-4b wasm e2e");
    }
}
