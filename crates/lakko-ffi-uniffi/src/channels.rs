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
//! let kp        = ChannelSubscriberKeypair.generate()   // Ed25519 (ADR-0060)
//! let envelope  = try sealSubscriberSeedEnvelope(
//!     subscriberPub: kp.public(),                        // the Ed25519 public key
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
    compose_post, decode_post, member_card_epoch as inner_member_card_epoch,
    meta::{
        channel_meta_is_newer as inner_channel_meta_is_newer, classify_post_plaintext,
        compose_channel_meta as inner_compose_channel_meta, ChannelMeta, ChannelMetaError,
        PostContent,
    },
    open_member_card as inner_open_member_card, open_seed,
    post::ChannelPost as InnerPost,
    seal_member_card as inner_seal_member_card, seal_seed, sign_member_card_request,
    sign_unsubscribe_request, verify_and_decrypt, ContentKeySeed, EpochId, CHANNEL_ID_LEN,
    EXPORTER_LABEL_V1, MAX_MEMBER_CARD_PROFILE_LEN, MEMBER_CARD_SIG_HEADER, SEED_ENVELOPE_LEN,
    SUBSCRIBER_PK_LEN, UNSUBSCRIBE_SIG_HEADER,
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

// ---------------------------------------------------------
// MLS-derived seed (G4-mls-seed) — production path
// ---------------------------------------------------------

/// Wrap raw bytes from `lakko_mls::Group::export_secret`
/// into the canonical channel content-key seed format.
///
/// **Production path.** The caller is expected to:
///
/// 1. Hold an MLS PublisherGroup (created via the UniFFI
///    `Group` surface).
/// 2. Invoke `group.exportSecret(identity, label,
///    channelId, 32)` with `label =
///    channelExporterLabel()` (`"lakko/channel/v1"`) and
///    `channelId` as the 16-byte channel pseudonym.
/// 3. Pass the resulting 32 bytes through this function —
///    we validate the length and return them as the
///    `seed` argument every other channels surface
///    (`composeChannelPost`, `verifyAndDecryptChannelPost`,
///    `sealSubscriberSeedEnvelope`) expects.
///
/// The pass-through is intentional: keeping the wrapper
/// at the FFI boundary instead of letting the JS / Swift
/// / Kotlin sides call `composeChannelPost` directly with
/// the MLS bytes makes the production path **grep-clean**
/// (`channelSeedFromMlsExporterOutput` is the audit
/// marker), and lets a future schema bump add a domain
/// separator without breaking call sites.
///
/// # Errors
///
/// [`FfiError::Channels`] if `bytes.len() != 32`.
#[uniffi::export]
pub fn channel_seed_from_mls_exporter_output(bytes: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    // Round-trip through ContentKeySeed::from_exporter_output
    // to surface a uniform error variant + reuse the
    // length validation that the in-Rust call sites use.
    let seed = ContentKeySeed::from_exporter_output(&bytes)?;
    Ok(seed.copy_to_bytes_for_export().to_vec())
}

/// Canonical MLS exporter label for channel content-key
/// seeds — `"lakko/channel/v1"`. Pinned ASCII, never
/// translated, never re-encoded.
///
/// Use as the `label` argument to
/// `lakko_mls::Group::export_secret` when deriving a
/// channel seed. The `_v1` suffix is part of the wire
/// contract; a future schema bump that changes the
/// derivation surfaces as `_v2` with the old label
/// frozen for backwards-compat.
#[uniffi::export]
#[must_use]
pub fn channel_exporter_label() -> String {
    EXPORTER_LABEL_V1.into()
}

// =========================================================
// Subscriber-side surface (G4-3c) — HPKE seed envelope
// =========================================================

const SUBSCRIBER_SEED_LEN: usize = 32;

/// The subscriber's key pair (ADR-0060: Ed25519). The public
/// half is the subscriber identifier everywhere - what gets
/// registered on `POST /v1/channels/:id/subscribers`, the
/// envelope and member-card path key, and the key member-card
/// requests are signed with. The seed envelope is sealed to
/// its X25519 image, and opened with the X25519 secret
/// derived from the same seed.
///
/// Like [`ChannelSigningKey`], deliberately not `Debug` —
/// a Debug impl would leak the 32-byte seed in panic
/// messages.
#[derive(uniffi::Object)]
pub struct ChannelSubscriberKeypair {
    inner: Mutex<SigningKey>,
}

#[uniffi::export]
impl ChannelSubscriberKeypair {
    /// Fresh random Ed25519 key pair. Backed by `getrandom`
    /// (system CSPRNG on both iOS and Android).
    #[uniffi::constructor]
    #[must_use]
    pub fn generate() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            inner: Mutex::new(SigningKey::generate()),
        })
    }

    /// Reconstruct from the 32-byte Ed25519 seed [`Self::secret`]
    /// exported.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] if `secret.len() != 32`.
    #[uniffi::constructor]
    pub fn from_secret(secret: Vec<u8>) -> Result<std::sync::Arc<Self>, FfiError> {
        let arr: [u8; SUBSCRIBER_SEED_LEN] = secret
            .as_slice()
            .try_into()
            .map_err(|_| FfiError::Channels("subscriber secret must be exactly 32 bytes".into()))?;
        Ok(std::sync::Arc::new(Self {
            inner: Mutex::new(SigningKey::from_seed(&arr)),
        }))
    }

    /// Export the 32-byte Ed25519 seed for persistence.
    /// Caller-side responsibility to keep this in a secure
    /// store (Keychain / Keystore).
    #[uniffi::method]
    #[must_use]
    pub fn secret(&self) -> Vec<u8> {
        self.inner
            .lock()
            .expect("subscriber-keypair lock")
            .to_seed()
            .to_vec()
    }

    /// 32-byte Ed25519 public key — the value the
    /// subscriber registers on the channel.
    #[uniffi::method]
    #[must_use]
    pub fn public(&self) -> Vec<u8> {
        self.inner
            .lock()
            .expect("subscriber-keypair lock")
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    /// The [`member_card_sig_header_name`] header value for
    /// uploading `card` (`PUT .../members/<public hex>`) or,
    /// with an empty `card`, deleting it: the Ed25519
    /// signature over `lakko_channels::member_card_signed_bytes`,
    /// base64url without padding.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] on a wrong-length `channel_id`.
    #[uniffi::method]
    pub fn member_card_request_signature(
        &self,
        channel_id: Vec<u8>,
        card: Vec<u8>,
    ) -> Result<String, FfiError> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let channel = channel_id_from_vec(channel_id)?;
        let key = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(URL_SAFE_NO_PAD.encode(sign_member_card_request(&key, &channel, &card)))
    }

    /// The [`unsubscribe_sig_header_name`] header value for leaving
    /// `channel_id` (`DELETE /v1/channels/<id>/subscribers/<public hex>`,
    /// ADR-0060): `<issued_at_ms>.<signature base64url>`, the Ed25519
    /// signature over `lakko_channels::unsubscribe_signed_bytes` stamped
    /// `issued_at_ms` (the caller's clock, UNIX ms). The server accepts it
    /// within `lakko_channels::UNSUBSCRIBE_WINDOW_MS` of its own clock.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] on a wrong-length `channel_id`.
    #[uniffi::method]
    pub fn unsubscribe_request_header(
        &self,
        channel_id: Vec<u8>,
        issued_at_ms: u64,
    ) -> Result<String, FfiError> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let channel = channel_id_from_vec(channel_id)?;
        let key = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let sig = sign_unsubscribe_request(&key, &channel, issued_at_ms);
        Ok(format!("{issued_at_ms}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }
}

/// The request header an unsubscribe carries its signature in
/// (`x-lakko-unsubscribe-sig`).
#[uniffi::export]
#[must_use]
pub fn unsubscribe_sig_header_name() -> String {
    UNSUBSCRIBE_SIG_HEADER.to_string()
}

/// The request header a member-card upload or delete carries
/// its subscriber signature in (`x-lakko-member-card-sig`).
#[uniffi::export]
#[must_use]
pub fn member_card_sig_header_name() -> String {
    MEMBER_CARD_SIG_HEADER.to_string()
}

/// Seal a 32-byte content-key seed against a subscriber's
/// Ed25519 public key (sealed to its X25519 image). Returns
/// the wire bytes ready to
/// `POST /v1/channels/:id/subscribers/:pk/envelope`.
///
/// Always returns exactly [`channel_seed_envelope_len`]
/// bytes (`eph_pub(32) || sealed_seed(32) || tag(16) = 80`).
///
/// # Errors
///
/// [`FfiError::Channels`] on:
/// - wrong-length `subscriber_pub` / `channel_id` / `seed`
/// - `subscriber_pub` not a valid Ed25519 point, or of small
///   order
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
    let pub_arr = subscriber_pk_from_vec(&subscriber_pub)?;
    let seed_arr: [u8; CONTENT_KEY_SEED_LEN] = seed
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Channels("content-key seed must be exactly 32 bytes".into()))?;
    let seed_handle = ContentKeySeed::from_bytes_for_test(seed_arr);
    Ok(seal_seed(
        &pub_arr,
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

// =========================================================
// Channel metadata record (ADR-0051 §4)
// =========================================================
//
// The record is the PLAINTEXT of an ordinary post: the client
// composes it here, then hands the bytes to `composeChannelPost`
// like any message. On receive, the client runs
// `classifyChannelPostPlaintext` on every decrypted post BEFORE
// the text path, keeps the newest `Meta` per channel by
// `channelMetaIsNewer`, drops `Malformed`, and renders `Text`.

impl From<ChannelMetaError> for FfiError {
    fn from(e: ChannelMetaError) -> Self {
        Self::ChannelMeta(format!("{}: {e}", e.code()))
    }
}

/// A channel's name and description as the publisher stated
/// them (ADR-0051 §4). Both are the publisher's own statement,
/// trusted because the carrying post's signature verified.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiChannelMeta {
    /// Trimmed, 1 to 64 Unicode scalars, at most 255 bytes, one line,
    /// no control / bidi / invisible format characters.
    pub name: String,
    /// Trimmed, 0 to 300 scalars, at most 1024 bytes; LINE FEED
    /// permitted, otherwise the name's character rules.
    pub description: String,
    /// Publisher-clock UNIX milliseconds. A re-issue (after a
    /// rotation, or for retention) MUST carry the stored value
    /// verbatim, never a fresh clock read.
    pub updated_at_ms: u64,
    /// The channel's topic, a `channelTopics()` id (ADR-0059 §2). Any value
    /// is admissible; an unknown one reads as "Other". Defaults to 0
    /// ("Other") so existing Swift / Kotlin initialisers keep compiling.
    #[uniffi(default = 0)]
    pub topic: u8,
    /// ISO 639 language code of 2 or 3 ASCII lowercase letters, or empty for
    /// "not set" (the default).
    #[uniffi(default = "")]
    pub language: String,
    /// Whether the owner lists the channel in the directory (ADR-0059 §2).
    /// A member shares only listed channels in a group. Defaults to false.
    #[uniffi(default = false)]
    pub listed: bool,
}

impl From<ChannelMeta> for FfiChannelMeta {
    fn from(m: ChannelMeta) -> Self {
        Self {
            name: m.name,
            description: m.description,
            updated_at_ms: m.updated_at_ms,
            topic: m.topic,
            language: m.language,
            listed: m.listed,
        }
    }
}

impl From<FfiChannelMeta> for ChannelMeta {
    fn from(m: FfiChannelMeta) -> Self {
        Self {
            name: m.name,
            description: m.description,
            updated_at_ms: m.updated_at_ms,
            topic: m.topic,
            language: m.language,
            listed: m.listed,
        }
    }
}

/// What a decrypted post plaintext turned out to be.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiChannelPostKind {
    /// Not a metadata record; render `text` through the text path.
    Text,
    /// A well-formed, in-bounds metadata record in `meta`.
    Meta,
    /// The record domain matched but the record failed a bound;
    /// `error` carries the frozen reason code. Drop it, never
    /// render it as text.
    Malformed,
}

/// Result of [`classify_channel_post_plaintext`]. A flat record
/// rather than an enum with data: exactly one of `text` / `meta` /
/// `error` is set, selected by `kind`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiChannelPostContent {
    /// Which of the three fields below is populated.
    pub kind: FfiChannelPostKind,
    /// The plaintext, verbatim, when `kind == Text`.
    pub text: Option<Vec<u8>>,
    /// The decoded record when `kind == Meta`.
    pub meta: Option<FfiChannelMeta>,
    /// The frozen `ChannelMetaError::code()` when `kind == Malformed`
    /// (for example `name.empty`, `unknownVersion`).
    pub error: Option<String>,
}

/// Compose a channel metadata record — the PLAINTEXT to hand to
/// [`compose_channel_post`]. Trim the inputs first; the composer
/// refuses untrimmed text exactly as every reader's decoder does.
///
/// # Errors
///
/// [`FfiError::ChannelMeta`] with the frozen reason code first in
/// the message (`name.empty`, `name.tooLong`, `name.lineBreak`,
/// `name.controlOrFormatCharacter`, `name.tooManyBytes`,
/// `name.notTrimmed`, and the same under `description.`).
#[uniffi::export]
pub fn compose_channel_meta(meta: FfiChannelMeta) -> Result<Vec<u8>, FfiError> {
    Ok(inner_compose_channel_meta(&ChannelMeta::from(meta))?)
}

/// Classify a decrypted post plaintext: run this on EVERY post
/// before the text path. Never fails — an unrecognised plaintext is
/// `Text`, a matching domain that fails a bound is `Malformed`.
#[uniffi::export]
#[must_use]
pub fn classify_channel_post_plaintext(plaintext: Vec<u8>) -> FfiChannelPostContent {
    match classify_post_plaintext(&plaintext) {
        PostContent::Text(text) => FfiChannelPostContent {
            kind: FfiChannelPostKind::Text,
            text: Some(text.to_vec()),
            meta: None,
            error: None,
        },
        PostContent::Meta(meta) => FfiChannelPostContent {
            kind: FfiChannelPostKind::Meta,
            text: None,
            meta: Some(meta.into()),
            error: None,
        },
        PostContent::Malformed(e) => FfiChannelPostContent {
            kind: FfiChannelPostKind::Malformed,
            text: None,
            meta: None,
            error: Some(e.code()),
        },
    }
}

/// `true` when a record stamped `incoming_updated_at_ms` must replace
/// the stored one for the same channel. Strictly greater; equal is
/// NOT newer, so a re-issue of the record already held is not an
/// update.
#[uniffi::export]
#[must_use]
pub fn channel_meta_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    inner_channel_meta_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

// =========================================================
// Channel member card (ADR-0060 §3)
// =========================================================
//
// A subscriber's (or publisher's) profile envelope sealed under the channel's
// current epoch seed, uploaded to `PUT /v1/channels/:id/members/:pk`. The
// seed is the same 32-byte content-key seed the post functions above take;
// re-upload after every epoch rotation. A reader picks the seed for the
// card's clear epoch with `memberCardEpoch`, then opens it.

fn content_seed(seed: &[u8]) -> Result<ContentKeySeed, FfiError> {
    let arr: [u8; CONTENT_KEY_SEED_LEN] = seed
        .try_into()
        .map_err(|_| FfiError::Channels("content-key seed must be exactly 32 bytes".into()))?;
    Ok(ContentKeySeed::from_bytes_for_test(arr))
}

fn subscriber_pk_from_vec(pk: &[u8]) -> Result<[u8; SUBSCRIBER_PK_LEN], FfiError> {
    pk.try_into().map_err(|_| {
        FfiError::Channels(format!(
            "subscriber key must be exactly {SUBSCRIBER_PK_LEN} bytes, got {}",
            pk.len()
        ))
    })
}

/// Seal `profile` (a profile envelope, ADR-0048 v3) into a member card for
/// `subscriber_pk` in `channel_id` under the 32-byte seed of `epoch_id`,
/// bound to the account whose 32-byte Ed25519 identity seed is
/// `account_identity_seed` (ADR-0060: the card names the account, signed).
///
/// # Errors
///
/// [`FfiError::Channels`] for a wrong-length seed, channel id, subscriber
/// key or identity seed, or a profile over [`max_member_card_profile_len`]
/// bytes.
#[uniffi::export]
pub fn seal_member_card(
    seed: Vec<u8>,
    epoch_id: u32,
    channel_id: Vec<u8>,
    subscriber_pk: Vec<u8>,
    account_identity_seed: Vec<u8>,
    profile: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel = channel_id_from_vec(channel_id)?;
    let pk = subscriber_pk_from_vec(&subscriber_pk)?;
    let identity: [u8; 32] = account_identity_seed
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Channels("account identity seed must be exactly 32 bytes".into()))?;
    Ok(inner_seal_member_card(
        &content_seed(&seed)?,
        EpochId::new(epoch_id),
        &channel,
        &pk,
        &identity,
        &profile,
    )?)
}

/// A member card opened and its account binding verified.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct OpenedMemberCard {
    /// The 32-byte account identity public key that signed the card: file
    /// the profile and any grant under this account, never under the
    /// subscriber key.
    pub identity_pk: Vec<u8>,
    /// The profile envelope; decode it with `decodeProfileEnvelope`.
    pub profile: Vec<u8>,
}

/// The clear epoch of a member card, to pick the matching seed before
/// [`open_member_card`]. Not authenticated until the card opens.
///
/// # Errors
///
/// [`FfiError::Channels`] for a malformed card.
#[uniffi::export]
pub fn member_card_epoch(card: Vec<u8>) -> Result<u32, FfiError> {
    Ok(inner_member_card_epoch(&card)?.as_u32())
}

/// Open a member card served for `subscriber_pk` in `channel_id` with the
/// 32-byte seed of the epoch it names and verify its account binding;
/// returns the account identity key and the profile envelope, which the
/// caller decodes with `decodeProfileEnvelope`. Fail-closed: a card sealed
/// for another subscriber, channel or epoch, or naming an account that did
/// not sign it, does not open.
///
/// # Errors
///
/// [`FfiError::Channels`] for a malformed card, a wrong-length argument, a
/// card the authenticator refuses, or a binding that does not verify.
#[uniffi::export]
pub fn open_member_card(
    seed: Vec<u8>,
    card: Vec<u8>,
    channel_id: Vec<u8>,
    subscriber_pk: Vec<u8>,
) -> Result<OpenedMemberCard, FfiError> {
    let channel = channel_id_from_vec(channel_id)?;
    let pk = subscriber_pk_from_vec(&subscriber_pk)?;
    let opened = inner_open_member_card(&content_seed(&seed)?, &card, &channel, &pk)?;
    Ok(OpenedMemberCard {
        identity_pk: opened.identity_pk.to_vec(),
        profile: opened.profile,
    })
}

/// Longest profile a member card seals, in bytes.
#[uniffi::export]
#[must_use]
pub fn max_member_card_profile_len() -> u32 {
    u32::try_from(MAX_MEMBER_CARD_PROFILE_LEN).unwrap_or(u32::MAX)
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
    #[test]
    fn member_card_seals_and_opens_through_the_boundary() {
        let seed = vec![0x33; 32];
        let channel = vec![0x44; 16];
        let pk = vec![0x55; 32];
        let account = vec![0x66; 32];
        let card = super::seal_member_card(
            seed.clone(),
            7,
            channel.clone(),
            pk.clone(),
            account.clone(),
            b"profile".to_vec(),
        )
        .unwrap();
        assert_eq!(super::member_card_epoch(card.clone()).unwrap(), 7);
        let opened =
            super::open_member_card(seed.clone(), card.clone(), channel.clone(), pk).unwrap();
        assert_eq!(opened.profile, b"profile".to_vec());
        assert_eq!(
            opened.identity_pk,
            lakko_crypto::signing::SigningKey::from_seed(&[0x66; 32])
                .verifying_key()
                .to_bytes()
                .to_vec()
        );
        assert!(matches!(
            super::seal_member_card(
                seed.clone(),
                7,
                channel.clone(),
                vec![0x55; 32],
                vec![0x66; 31],
                b"p".to_vec()
            ),
            Err(crate::FfiError::Channels(_))
        ));
        // Served under another subscriber key, the card does not open.
        assert!(matches!(
            super::open_member_card(seed.clone(), card, channel.clone(), vec![0x56; 32]),
            Err(crate::FfiError::Channels(_))
        ));
        assert!(matches!(
            super::seal_member_card(
                seed,
                7,
                channel,
                vec![0x55; 32],
                account,
                vec![0; super::max_member_card_profile_len() as usize + 1],
            ),
            Err(crate::FfiError::Channels(_))
        ));
        assert!(super::member_card_epoch(vec![1, 2, 3]).is_err());
    }

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
    // MLS-derived seed (G4-mls-seed) tests
    // -----------------------------------------------------

    #[test]
    fn channel_exporter_label_pinned() {
        // Wire-contract pin — if the label string ever
        // changes, every previously-published channel's
        // seed derivation no longer matches and the
        // subscriber path silently fails. The audit
        // marker for any change here is the `_v1`
        // suffix.
        assert_eq!(channel_exporter_label(), "lakko/channel/v1");
    }

    #[test]
    fn mls_exporter_output_round_trip_when_32_bytes() {
        let raw = vec![0x5Au8; CONTENT_KEY_SEED_LEN];
        let seed = channel_seed_from_mls_exporter_output(raw.clone()).unwrap();
        assert_eq!(seed, raw);
    }

    #[test]
    fn mls_exporter_output_rejects_wrong_length() {
        for len in [0usize, 1, 16, 31, 33, 64] {
            let err = channel_seed_from_mls_exporter_output(vec![0u8; len]).unwrap_err();
            assert!(matches!(err, FfiError::Channels(_)));
        }
    }

    #[test]
    fn mls_exporter_output_full_compose_round_trip() {
        // End-to-end: pretend an MLS exporter yielded
        // these 32 bytes; feed them through the
        // production FFI helper; use the result as the
        // seed for compose / verify_and_decrypt.
        let mls_bytes = vec![0xC3u8; CONTENT_KEY_SEED_LEN];
        let seed = channel_seed_from_mls_exporter_output(mls_bytes).unwrap();
        let signer = ChannelSigningKey::generate();
        let channel_id = vec![0x42u8; CHANNEL_ID_LEN];

        let wire = compose_channel_post(
            seed.clone(),
            channel_id.clone(),
            1,
            signer.clone(),
            0,
            b"mls-derived seed end-to-end".to_vec(),
        )
        .unwrap();
        let post = decode_channel_post(wire).unwrap();
        let pt = verify_and_decrypt_channel_post(seed, channel_id, post).unwrap();
        assert_eq!(pt, b"mls-derived seed end-to-end");
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
        assert_eq!(kp.public().len(), SUBSCRIBER_PK_LEN);
    }

    #[test]
    fn the_subscriber_key_is_ed25519_and_signs_member_card_requests() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;
        let kp = ChannelSubscriberKeypair::generate();
        // The public half is the Ed25519 key of the exported seed.
        let seed: [u8; 32] = kp.secret().try_into().unwrap();
        assert_eq!(
            kp.public(),
            SigningKey::from_seed(&seed)
                .verifying_key()
                .to_bytes()
                .to_vec()
        );
        let channel = vec![0x0Cu8; CHANNEL_ID_LEN];
        let card = vec![0x01u8; 40];
        let header = kp
            .member_card_request_signature(channel.clone(), card.clone())
            .unwrap();
        let sig = URL_SAFE_NO_PAD.decode(header).unwrap();
        let pk: [u8; 32] = kp.public().try_into().unwrap();
        lakko_channels::verify_member_card_request(
            &channel.clone().try_into().unwrap(),
            &pk,
            &card,
            &sig,
        )
        .unwrap();
        assert_eq!(member_card_sig_header_name(), "x-lakko-member-card-sig");

        // The unsubscribe header verifies with the core verifier.
        let header = kp
            .unsubscribe_request_header(channel.clone(), 1_000)
            .unwrap();
        let (stamp, sig) = header.split_once('.').unwrap();
        assert_eq!(stamp, "1000");
        lakko_channels::verify_unsubscribe_request(
            &channel.clone().try_into().unwrap(),
            &pk,
            1_000,
            &URL_SAFE_NO_PAD.decode(sig).unwrap(),
            1_000,
        )
        .unwrap();
        assert_eq!(unsubscribe_sig_header_name(), "x-lakko-unsubscribe-sig");
        // A small-order key is refused at seal.
        let mut identity = vec![0u8; 32];
        identity[0] = 1;
        assert!(matches!(
            seal_subscriber_seed_envelope(identity, channel, 1, vec![0x33; 32]),
            Err(FfiError::Channels(_))
        ));
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

    // -----------------------------------------------------
    // Channel metadata record (ADR-0051 §4) tests
    // -----------------------------------------------------

    fn ffi_meta(name: &str, description: &str, at: u64) -> FfiChannelMeta {
        FfiChannelMeta {
            name: name.to_string(),
            description: description.to_string(),
            updated_at_ms: at,
            topic: 2,
            language: "en".to_string(),
            listed: true,
        }
    }

    #[test]
    fn channel_meta_compose_then_classify_round_trips() {
        let meta = ffi_meta("Lakko News", "Releases\nand outages", 1_700_000_000_000);
        let wire = compose_channel_meta(meta.clone()).unwrap();
        let content = classify_channel_post_plaintext(wire);
        assert_eq!(content.kind, FfiChannelPostKind::Meta);
        assert_eq!(content.meta, Some(meta));
        assert!(content.text.is_none());
        assert!(content.error.is_none());
    }

    #[test]
    fn channel_meta_text_post_classifies_as_text_verbatim() {
        let text = b"hello subscribers".to_vec();
        let content = classify_channel_post_plaintext(text.clone());
        assert_eq!(content.kind, FfiChannelPostKind::Text);
        assert_eq!(content.text, Some(text));
        assert!(content.meta.is_none());
        assert!(content.error.is_none());
        // The printable domain without the NUL is still text.
        let lookalike = b"lakko/channel-meta/v1".to_vec();
        let content = classify_channel_post_plaintext(lookalike.clone());
        assert_eq!(content.kind, FfiChannelPostKind::Text);
        assert_eq!(content.text, Some(lookalike));
    }

    #[test]
    fn channel_meta_malformed_record_carries_the_code() {
        // A bare separator: the domain matched, the payload is missing.
        let content =
            classify_channel_post_plaintext(lakko_channels::CHANNEL_META_DOMAIN_V1.to_vec());
        assert_eq!(content.kind, FfiChannelPostKind::Malformed);
        assert_eq!(content.error.as_deref(), Some("truncated"));
        assert!(content.text.is_none());
        assert!(content.meta.is_none());
    }

    #[test]
    fn channel_meta_compose_refuses_out_of_bounds_with_the_code_first() {
        let err = compose_channel_meta(ffi_meta("", "", 0)).unwrap_err();
        match err {
            FfiError::ChannelMeta(msg) => assert!(msg.starts_with("name.empty: "), "{msg}"),
            other => panic!("expected ChannelMeta, got {other:?}"),
        }
        let err = compose_channel_meta(ffi_meta("ok", "bad\u{202E}", 0)).unwrap_err();
        match err {
            FfiError::ChannelMeta(msg) => assert!(
                msg.starts_with("description.controlOrFormatCharacter: "),
                "{msg}"
            ),
            other => panic!("expected ChannelMeta, got {other:?}"),
        }
    }

    #[test]
    fn channel_meta_is_newer_is_strict() {
        assert!(channel_meta_is_newer(2, 1));
        assert!(!channel_meta_is_newer(1, 1));
        assert!(!channel_meta_is_newer(0, 1));
    }

    #[test]
    fn channel_meta_record_survives_the_full_post_pipeline() {
        // The record is the plaintext of an ordinary post: compose the
        // record, seal it as a post, decode + verify on the subscriber
        // side, classify the recovered plaintext.
        let signer = ChannelSigningKey::generate();
        let seed = new_channel_seed_for_test();
        let channel_id = vec![0x51u8; CHANNEL_ID_LEN];
        let meta = ffi_meta("Lakko News", "", 42);
        let record = compose_channel_meta(meta.clone()).unwrap();
        let wire =
            compose_channel_post(seed.clone(), channel_id.clone(), 1, signer, 0, record).unwrap();
        let post = decode_channel_post(wire).unwrap();
        let plaintext = verify_and_decrypt_channel_post(seed, channel_id, post).unwrap();
        let content = classify_channel_post_plaintext(plaintext);
        assert_eq!(content.kind, FfiChannelPostKind::Meta);
        assert_eq!(content.meta, Some(meta));
    }
}
