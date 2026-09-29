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
    compose_post, decode_post,
    meta::{
        channel_meta_is_newer as inner_channel_meta_is_newer, classify_post_plaintext,
        compose_channel_meta as inner_compose_channel_meta, ChannelMeta, ChannelMetaError,
        PostContent,
    },
    open_seed,
    post::ChannelPost as RustChannelPost,
    seal_seed, verify_and_decrypt, ContentKeySeed, EpochId, CHANNEL_ID_LEN, EXPORTER_LABEL_V1,
    SEED_ENVELOPE_LEN,
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

// ---------------------------------------------------------
// MLS-derived seed (G4-mls-seed) — production path
// ---------------------------------------------------------

/// Wrap raw bytes from `Group.exportSecret` into the
/// canonical channel content-key seed format.
///
/// **Production path.** The caller is expected to hold a
/// PublisherGroup (created via the wasm `Group` surface),
/// invoke `group.exportSecret(identity,
/// channelExporterLabel(), channelIdBytes, 32)`, and feed
/// the resulting 32 bytes through this function. The
/// pass-through gives the production seed a grep-clean
/// audit marker (`channelSeedFromMlsExporterOutput`) and
/// lets a future schema bump add a domain separator
/// without breaking call sites.
///
/// Throws on `bytes.length !== 32`.
#[wasm_bindgen(js_name = "channelSeedFromMlsExporterOutput")]
pub fn channel_seed_from_mls_exporter_output(bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    let seed = ContentKeySeed::from_exporter_output(bytes)
        .map_err(|e| JsError::new(&format!("from_exporter_output: {e}")))?;
    Ok(seed.copy_to_bytes_for_export().to_vec())
}

/// Canonical MLS exporter label for channel content-key
/// seeds — `"lakko/channel/v1"`. Pinned ASCII, never
/// translated, never re-encoded.
///
/// Use as the `label` argument to `Group.exportSecret`
/// when deriving a channel seed. The `_v1` suffix is
/// part of the wire contract.
#[wasm_bindgen(js_name = "channelExporterLabel")]
#[must_use]
pub fn channel_exporter_label() -> String {
    EXPORTER_LABEL_V1.into()
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

// =========================================================
// Channel metadata record (ADR-0051 §4)
// =========================================================
//
// The record is the PLAINTEXT of an ordinary post: the client
// composes it here, then hands the bytes to `composeChannelPost`
// like any message. On receive, the client runs
// `classifyChannelPostPlaintext` on every decrypted post BEFORE
// the text path, keeps the newest `"meta"` per channel by
// `channelMetaIsNewer`, drops `"malformed"`, and renders `"text"`.
//
// ```ts
// const record = composeChannelMeta("Lakko News", "Releases", BigInt(Date.now()));
// const wire   = composeChannelPost(seed, channelId, epoch, signer, counter, record);
// // ... subscriber side ...
// const c = classifyChannelPostPlaintext(plaintext);
// if (c.kind === "meta" && channelMetaIsNewer(c.updatedAtMs!, stored.updatedAtMs)) { ... }
// ```

/// The value thrown for a record that fails a bound: a real `Error`
/// carrying the frozen `ChannelMetaError::code()` as an own
/// `channelMetaError` property (`name.empty`, `description.tooLong`,
/// ...). A client selects copy from the code, never from the message;
/// the message never contains the input.
fn channel_meta_error_value(e: ChannelMetaError) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    // Best-effort, as in `discovery.rs`: a failed property write must not
    // replace the error being reported.
    let _ = js_sys::Reflect::set(
        err.as_ref(),
        &JsValue::from_str("channelMetaError"),
        &JsValue::from_str(&e.code()),
    );
    err.into()
}

/// What a decrypted post plaintext turned out to be. JS reads `kind`
/// first, then the getters that kind populates: `text` for `"text"`;
/// `name` / `description` / `updatedAtMs` for `"meta"`; `error` for
/// `"malformed"`. Every other getter is `undefined`.
#[wasm_bindgen(js_name = "ChannelPostContent")]
pub struct WasmChannelPostContent {
    kind: &'static str,
    text: Option<Vec<u8>>,
    meta: Option<ChannelMeta>,
    error: Option<String>,
}

#[wasm_bindgen(js_class = "ChannelPostContent")]
impl WasmChannelPostContent {
    /// `"text"` | `"meta"` | `"malformed"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> String {
        self.kind.to_string()
    }

    /// The plaintext, verbatim, when `kind === "text"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn text(&self) -> Option<Vec<u8>> {
        self.text.clone()
    }

    /// The channel name when `kind === "meta"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn name(&self) -> Option<String> {
        self.meta.as_ref().map(|m| m.name.clone())
    }

    /// The channel description (possibly empty) when `kind === "meta"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn description(&self) -> Option<String> {
        self.meta.as_ref().map(|m| m.description.clone())
    }

    /// The record's stamp as a BigInt when `kind === "meta"`. A re-issue
    /// MUST carry this value verbatim, never a fresh clock read.
    #[wasm_bindgen(getter, js_name = "updatedAtMs")]
    #[must_use]
    pub fn updated_at_ms(&self) -> Option<u64> {
        self.meta.as_ref().map(|m| m.updated_at_ms)
    }

    /// The topic id (ADR-0059 §2) when `kind === "meta"`; an unknown id
    /// reads as "Other" (`channelTopicName`).
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn topic(&self) -> Option<u8> {
        self.meta.as_ref().map(|m| m.topic)
    }

    /// The ISO 639 language when `kind === "meta"`; empty = not set.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn language(&self) -> Option<String> {
        self.meta.as_ref().map(|m| m.language.clone())
    }

    /// Whether the owner lists the channel in the directory, when
    /// `kind === "meta"`. A member shares only listed channels in a group.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn listed(&self) -> Option<bool> {
        self.meta.as_ref().map(|m| m.listed)
    }

    /// The frozen `ChannelMetaError::code()` when `kind === "malformed"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }
}

/// Compose a channel metadata record — the PLAINTEXT to hand to
/// `composeChannelPost`. Trim the inputs first; the composer refuses
/// untrimmed text exactly as every reader's decoder does.
///
/// # Errors
///
/// Throws an `Error` whose `channelMetaError` property is the frozen
/// reason code (`name.empty`, `name.tooLong`, `name.lineBreak`,
/// `name.controlOrFormatCharacter`, `name.tooManyBytes`,
/// `name.notTrimmed`, and the same under `description.`, plus
/// `language.badLength` / `language.notLowercaseAscii`).
///
/// `topic` (default 0, "Other"), `language` (default empty, "not set") and
/// `listed` (default false) are the ADR-0059 §2 fields of schema version 2.
#[wasm_bindgen(js_name = "composeChannelMeta")]
pub fn compose_channel_meta(
    name: &str,
    description: &str,
    updated_at_ms: u64,
    topic: Option<u8>,
    language: Option<String>,
    listed: Option<bool>,
) -> Result<Vec<u8>, JsValue> {
    let meta = ChannelMeta {
        name: name.to_string(),
        description: description.to_string(),
        updated_at_ms,
        topic: topic.unwrap_or(0),
        language: language.unwrap_or_default(),
        listed: listed.unwrap_or(false),
    };
    inner_compose_channel_meta(&meta).map_err(channel_meta_error_value)
}

/// Classify a decrypted post plaintext: run this on EVERY post before
/// the text path. Never throws — an unrecognised plaintext is `"text"`,
/// a matching domain that fails a bound is `"malformed"`.
#[wasm_bindgen(js_name = "classifyChannelPostPlaintext")]
#[must_use]
pub fn classify_channel_post_plaintext(plaintext: &[u8]) -> WasmChannelPostContent {
    match classify_post_plaintext(plaintext) {
        PostContent::Text(text) => WasmChannelPostContent {
            kind: "text",
            text: Some(text.to_vec()),
            meta: None,
            error: None,
        },
        PostContent::Meta(meta) => WasmChannelPostContent {
            kind: "meta",
            text: None,
            meta: Some(meta),
            error: None,
        },
        PostContent::Malformed(e) => WasmChannelPostContent {
            kind: "malformed",
            text: None,
            meta: None,
            error: Some(e.code()),
        },
    }
}

/// `true` when a record stamped `incomingUpdatedAtMs` must replace the
/// stored one for the same channel. Strictly greater; equal is NOT
/// newer, so a re-issue of the record already held is not an update.
#[wasm_bindgen(js_name = "channelMetaIsNewer")]
#[must_use]
pub fn channel_meta_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    inner_channel_meta_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
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
// Rotation policy (G4-2-rotation-timer)
// ---------------------------------------------------------
//
// wasm-bindgen mirror of `lakko_channels::rotation_policy`.
// Pure logic; the TypeScript caller drives a periodic
// `setInterval` (or any other clock source) that asks
// `channelShouldRotate(policy, state, Date.now())` and acts
// on the boolean.
//
// We keep the policy + state as **plain JS objects** rather
// than wasm-bindgen handle classes — they are 8-12 bytes
// total and copying them across the FFI is cheaper than
// the handle's vtable indirection.

/// JavaScript-side rotation policy. Mirrored from the Rust
/// `lakko_channels::RotationPolicy` (also exposed via
/// UniFFI as `ChannelRotationPolicy`).
#[wasm_bindgen]
extern "C" {
    /// TypeScript declares this shape:
    /// `{ intervalMs: bigint; maxPosts: number }`.
    #[wasm_bindgen(typescript_type = "ChannelRotationPolicy")]
    pub type ChannelRotationPolicy;
    /// TypeScript declares this shape:
    /// `{ lastRotationMs: bigint; postsSinceRotation: number }`.
    #[wasm_bindgen(typescript_type = "ChannelRotationState")]
    pub type ChannelRotationState;
}

#[wasm_bindgen(typescript_custom_section)]
const ROTATION_POLICY_TS: &'static str = r#"
/**
 * Rotation policy — when the publisher should rotate the
 * channel content key. Either trigger fires independently.
 * Mirrors `lakko_channels::rotation_policy::RotationPolicy`.
 */
export interface ChannelRotationPolicy {
  /** Max ms between rotations. 0 disables the time trigger. */
  intervalMs: bigint;
  /** Max posts between rotations. 0 disables the count trigger. */
  maxPosts: number;
}

/**
 * Caller-persisted snapshot of when the channel was last
 * rotated + how many posts since. Pair with
 * `channelShouldRotate(...)`.
 */
export interface ChannelRotationState {
  /** Unix ms when the last rotation completed. 0 = never. */
  lastRotationMs: bigint;
  /** Count of posts published since the last rotation. */
  postsSinceRotation: number;
}
"#;

/// Build the recommended default policy — 24 h or 1 000
/// posts, whichever lands first. Pinned in ADR-0010
/// §"PublisherGroup".
#[wasm_bindgen(js_name = channelRotationPolicyDefault)]
pub fn channel_rotation_policy_default_js() -> Result<ChannelRotationPolicy, JsError> {
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(
        &obj,
        &"intervalMs".into(),
        &js_sys::BigInt::from(lakko_channels::DEFAULT_INTERVAL_MS).into(),
    )
    .map_err(|e| JsError::new(&format!("set intervalMs: {e:?}")))?;
    js_sys::Reflect::set(
        &obj,
        &"maxPosts".into(),
        &JsValue::from_f64(f64::from(lakko_channels::DEFAULT_MAX_POSTS)),
    )
    .map_err(|e| JsError::new(&format!("set maxPosts: {e:?}")))?;
    Ok(obj.unchecked_into())
}

/// Genesis rotation state (`{ lastRotationMs: 0n,
/// postsSinceRotation: 0 }`).
#[wasm_bindgen(js_name = channelRotationStateNew)]
pub fn channel_rotation_state_new_js() -> Result<ChannelRotationState, JsError> {
    rotation_state_to_js(lakko_channels::RotationState::new())
}

/// Stamp a fresh rotation state right after a successful
/// fanout. Caller passes `Date.now()` (or any monotonic-
/// adjacent ms timestamp).
#[wasm_bindgen(js_name = channelRotationStateAfterRotation)]
pub fn channel_rotation_state_after_rotation_js(
    now_ms: u64,
) -> Result<ChannelRotationState, JsError> {
    rotation_state_to_js(lakko_channels::RotationState::after_rotation(now_ms))
}

/// Bump the post counter (saturating at `u32::MAX`).
#[wasm_bindgen(js_name = channelRotationStateAfterPost)]
pub fn channel_rotation_state_after_post_js(
    state: &ChannelRotationState,
) -> Result<ChannelRotationState, JsError> {
    let s = rotation_state_from_js(state)?;
    rotation_state_to_js(s.after_post())
}

/// Pure decision: should the publisher rotate now?
///
/// `now_ms` — caller-supplied epoch milliseconds (typically
/// `Date.now()`).
#[wasm_bindgen(js_name = channelShouldRotate)]
pub fn channel_should_rotate_js(
    policy: &ChannelRotationPolicy,
    state: &ChannelRotationState,
    now_ms: u64,
) -> Result<bool, JsError> {
    let p = rotation_policy_from_js(policy)?;
    let s = rotation_state_from_js(state)?;
    Ok(p.should_rotate(now_ms, s))
}

// Every `f64 as u{32,64}` below is guarded by an immediately-preceding range
// check (non-negative + finite, and `<= u32::MAX` for the u32 casts), so the
// truncation / sign-loss clippy flags are false positives here.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rotation_policy_from_js(
    policy: &ChannelRotationPolicy,
) -> Result<lakko_channels::RotationPolicy, JsError> {
    let val: JsValue = policy.into();
    let interval_ms = js_sys::Reflect::get(&val, &"intervalMs".into())
        .map_err(|e| JsError::new(&format!("read intervalMs: {e:?}")))?;
    let interval_ms: u64 = if interval_ms.is_bigint() {
        let bi: js_sys::BigInt = interval_ms.unchecked_into();
        u64::try_from(bi).map_err(|e| JsError::new(&format!("intervalMs to u64: {e:?}")))?
    } else if let Some(n) = interval_ms.as_f64() {
        if n < 0.0 || !n.is_finite() {
            return Err(JsError::new(
                "intervalMs must be a non-negative finite number or bigint",
            ));
        }
        n as u64
    } else {
        return Err(JsError::new("intervalMs must be bigint or number"));
    };
    let max_posts = js_sys::Reflect::get(&val, &"maxPosts".into())
        .map_err(|e| JsError::new(&format!("read maxPosts: {e:?}")))?
        .as_f64()
        .ok_or_else(|| JsError::new("maxPosts must be a number"))?;
    if !max_posts.is_finite() || max_posts < 0.0 || max_posts > f64::from(u32::MAX) {
        return Err(JsError::new("maxPosts out of u32 range"));
    }
    Ok(lakko_channels::RotationPolicy::new(
        interval_ms,
        max_posts as u32,
    ))
}

// Same guarded-cast posture as `rotation_policy_from_js` above.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rotation_state_from_js(
    state: &ChannelRotationState,
) -> Result<lakko_channels::RotationState, JsError> {
    let val: JsValue = state.into();
    let last = js_sys::Reflect::get(&val, &"lastRotationMs".into())
        .map_err(|e| JsError::new(&format!("read lastRotationMs: {e:?}")))?;
    let last: u64 = if last.is_bigint() {
        let bi: js_sys::BigInt = last.unchecked_into();
        u64::try_from(bi).map_err(|e| JsError::new(&format!("lastRotationMs to u64: {e:?}")))?
    } else if let Some(n) = last.as_f64() {
        if n < 0.0 || !n.is_finite() {
            return Err(JsError::new("lastRotationMs must be non-negative finite"));
        }
        n as u64
    } else {
        return Err(JsError::new("lastRotationMs must be bigint or number"));
    };
    let posts = js_sys::Reflect::get(&val, &"postsSinceRotation".into())
        .map_err(|e| JsError::new(&format!("read postsSinceRotation: {e:?}")))?
        .as_f64()
        .ok_or_else(|| JsError::new("postsSinceRotation must be a number"))?;
    if !posts.is_finite() || posts < 0.0 || posts > f64::from(u32::MAX) {
        return Err(JsError::new("postsSinceRotation out of u32 range"));
    }
    Ok(lakko_channels::RotationState {
        last_rotation_ms: last,
        posts_since_rotation: posts as u32,
    })
}

fn rotation_state_to_js(s: lakko_channels::RotationState) -> Result<ChannelRotationState, JsError> {
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(
        &obj,
        &"lastRotationMs".into(),
        &js_sys::BigInt::from(s.last_rotation_ms).into(),
    )
    .map_err(|e| JsError::new(&format!("set lastRotationMs: {e:?}")))?;
    js_sys::Reflect::set(
        &obj,
        &"postsSinceRotation".into(),
        &JsValue::from_f64(f64::from(s.posts_since_rotation)),
    )
    .map_err(|e| JsError::new(&format!("set postsSinceRotation: {e:?}")))?;
    Ok(obj.unchecked_into())
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
    // MLS-derived seed (G4-mls-seed) tests
    // -----------------------------------------------------

    #[test]
    fn channel_exporter_label_pinned_wasm_host() {
        // Wire-contract pin — see the UniFFI side for the
        // full rationale. Any change here breaks every
        // previously-derived channel seed.
        assert_eq!(channel_exporter_label(), "lakko/channel/v1");
    }

    #[test]
    fn mls_exporter_output_full_compose_round_trip_wasm_host() {
        let mls_bytes = vec![0x3Cu8; CONTENT_KEY_SEED_LEN];
        let seed = channel_seed_from_mls_exporter_output(&mls_bytes).unwrap();
        let signer = WasmChannelSigningKey::generate_js();
        let channel_id = [0x91u8; CHANNEL_ID_LEN];
        let wire = compose_channel_post(
            &seed,
            &channel_id,
            1,
            &signer,
            0,
            b"mls-derived seed wasm e2e",
        )
        .unwrap();
        let post = decode_channel_post(&wire).unwrap();
        let pt = verify_and_decrypt_channel_post(&seed, &channel_id, &post).unwrap();
        assert_eq!(pt, b"mls-derived seed wasm e2e");
    }

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

    // -----------------------------------------------------
    // Channel metadata record (ADR-0051 §4) tests
    // -----------------------------------------------------
    //
    // The throwing path of `composeChannelMeta` builds a
    // `js_sys::Error`, which cannot be constructed on a host
    // target (the same wasm-bindgen rule the negative-path note
    // above records), so only the positive path and the
    // never-throwing classifier run here; the bounds themselves
    // are pinned by `lakko-channels`'s own tests.

    #[test]
    fn channel_meta_compose_then_classify_round_trips_wasm_host() {
        let wire = compose_channel_meta(
            "Lakko News",
            "Releases\nand outages",
            1_700_000_000_000,
            Some(3),
            Some("en".to_string()),
            Some(true),
        )
        .unwrap();
        let content = classify_channel_post_plaintext(&wire);
        assert_eq!(content.kind(), "meta");
        assert_eq!(content.name().as_deref(), Some("Lakko News"));
        assert_eq!(
            content.description().as_deref(),
            Some("Releases\nand outages")
        );
        assert_eq!(content.updated_at_ms(), Some(1_700_000_000_000));
        assert_eq!(content.topic(), Some(3));
        assert_eq!(content.language().as_deref(), Some("en"));
        assert_eq!(content.listed(), Some(true));
        assert!(content.text().is_none());
        assert!(content.error().is_none());
    }

    #[test]
    fn channel_meta_text_post_classifies_as_text_verbatim_wasm_host() {
        let text = b"hello subscribers";
        let content = classify_channel_post_plaintext(text);
        assert_eq!(content.kind(), "text");
        assert_eq!(content.text().as_deref(), Some(&text[..]));
        assert!(content.name().is_none());
        assert!(content.description().is_none());
        assert!(content.updated_at_ms().is_none());
        assert!(content.error().is_none());
        // The printable domain without the NUL is still text.
        let lookalike = b"lakko/channel-meta/v1";
        let content = classify_channel_post_plaintext(lookalike);
        assert_eq!(content.kind(), "text");
        assert_eq!(content.text().as_deref(), Some(&lookalike[..]));
    }

    #[test]
    fn channel_meta_malformed_record_carries_the_code_wasm_host() {
        let content = classify_channel_post_plaintext(lakko_channels::CHANNEL_META_DOMAIN_V1);
        assert_eq!(content.kind(), "malformed");
        assert_eq!(content.error().as_deref(), Some("truncated"));
        assert!(content.text().is_none());
        assert!(content.name().is_none());
    }

    #[test]
    fn channel_meta_is_newer_is_strict_wasm_host() {
        assert!(channel_meta_is_newer(2, 1));
        assert!(!channel_meta_is_newer(1, 1));
        assert!(!channel_meta_is_newer(0, 1));
    }

    #[test]
    fn channel_meta_record_survives_the_full_post_pipeline_wasm_host() {
        let signer = WasmChannelSigningKey::generate_js();
        let seed = new_channel_seed_for_test();
        let channel_id = [0x51u8; CHANNEL_ID_LEN];
        let record = compose_channel_meta("Lakko News", "", 42, None, None, None).unwrap();
        let wire = compose_channel_post(&seed, &channel_id, 1, &signer, 0, &record).unwrap();
        let post = decode_channel_post(&wire).unwrap();
        let plaintext = verify_and_decrypt_channel_post(&seed, &channel_id, &post).unwrap();
        let content = classify_channel_post_plaintext(&plaintext);
        assert_eq!(content.kind(), "meta");
        assert_eq!(content.name().as_deref(), Some("Lakko News"));
        assert_eq!(content.description().as_deref(), Some(""));
        assert_eq!(content.updated_at_ms(), Some(42));
    }
}
