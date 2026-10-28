// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Push without a bipartite graph ([ADR-0078] section 3).
//!
//! ## Why
//!
//! Before ADR-0078 push-gw stored `push_tokens (device_id, provider, token)`
//! and `push_group_subscriptions (device_id, group_id)`: one device pseudonym
//! subscribed to all of its groups, so push-gw alone held the devices x
//! groups graph, and the Delivery Service stored a peppered binding of every
//! group's `push_gid` to its journal (the pepper sat in the same environment
//! file), so each group was also the journal. This module carries the two
//! wire formats that remove both:
//!
//! * **The sealed wake.** A member asking the DS to wake a group puts a
//!   sealed box to push-gw's X25519 key into `x-lakko-wake`. The DS forwards
//!   it unread. Inside: the `push_gid`, the kind, a nonce, a timestamp, the
//!   group's **unlock key** and a signature by the group's **wake key**,
//!   both derived from the group's push routing secret
//!   ([`crate::push_routing::derive_push_wake_signing_seed`],
//!   [`crate::push_routing::derive_push_wake_unlock_key`]). Only a member
//!   holds the secret, so only a member can produce a wake push-gw accepts:
//!   that replaces the DS-side journal binding the anti-spoofing used to
//!   rest on.
//! * **The sealed subscription.** A subscription row at push-gw is
//!   `(sub_id, push_gid, wake_pk, sealed)`, where `sealed` is the device's
//!   provider tokens AEAD-sealed under the group's unlock key. The unlock key
//!   reaches push-gw only inside a sealed wake and is never stored, so the
//!   push-gw database at rest holds no token in the clear, nothing equal
//!   across two groups, and no device pseudonym. `sub_id` is 16 random bytes
//!   a device keeps per subscription so it can remove its own row.
//!
//! What this does **not** remove (class (b), stated in the ADR): a live
//! push-gw opens the rows a wake names and sends to their tokens, so it sees
//! which tokens are woken together.
//!
//! ## Wire formats
//!
//! ```text
//! wake (234)  = version(1)=0x01 ‖ eph_x25519_pk(32) ‖ AEAD(plaintext(185)) (+16 tag)
//!   AAD       = "lakko/push-wake/v1\0" ‖ version
//!   plaintext = push_gid(32) ‖ kind(1) ‖ nonce(16) ‖ timestamp_ms(8 BE)
//!               ‖ unlock_key(32) ‖ wake_pk(32) ‖ sig(64)
//!   sig       = Ed25519_wake("lakko/push/wake-sig/v1\0" ‖ every field before it)
//!
//! subscription = version(1)=0x01 ‖ nonce(12) ‖ ChaCha20-Poly1305(unlock_key, plaintext)
//!   AAD       = "lakko/push-subscription/v1\0" ‖ version ‖ push_gid(32) ‖ sub_id(16)
//!   plaintext = count(1) ‖ count x (provider(1) ‖ token_len(2 BE) ‖ token) ‖ zeros,
//!               zero-padded to a multiple of 256 bytes
//! ```
//!
//! The wake's AEAD is the [`crate::envelope`] sealed box under its own HKDF
//! labels, so a wake can never be opened as a mailbox row or a contact
//! request's seal, or the other way round.
//!
//! A subscription carries a **list** of tokens rather than one: an iOS device
//! holds an alert token and a call token, and push-gw's delivery rule (a call
//! goes to the call token when there is one, a message never does) needs the
//! device's whole set, which a row per token would hand to push-gw only as a
//! cross-row join on a device identifier this format exists to remove.
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use core::fmt;
use core::result::Result as CoreResult;

use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, NONCE_LEN};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::random;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};
use lakko_crypto::SecretBytes;
use thiserror::Error;

use crate::envelope::{self, SealLabels, EPH_PUB_LEN, TAG_LEN};
use crate::push_routing::{
    derive_push_gid, derive_push_wake_signing_seed, derive_push_wake_unlock_key, PUSH_GID_LEN,
    PUSH_ROUTING_SECRET_LEN, PUSH_WAKE_KEY_LEN,
};

/// Wake schema version.
pub const PUSH_WAKE_VERSION_V1: u8 = 0x01;

/// Domain separator that opens a wake's AAD. Frozen.
pub const PUSH_WAKE_AAD_DOMAIN: &[u8] = b"lakko/push-wake/v1\x00";

/// Domain separator the wake key signs under. Frozen.
pub const PUSH_WAKE_SIG_DOMAIN: &[u8] = b"lakko/push/wake-sig/v1\x00";

/// Length of a wake's nonce (what push-gw deduplicates on).
pub const PUSH_WAKE_NONCE_LEN: usize = 16;

/// Length of push-gw's X25519 public key.
pub const PUSH_GW_KEY_LEN: usize = 32;

/// Ed25519 signature length.
const SIGNATURE_LEN: usize = 64;

/// Length of the signed part of a wake plaintext (everything but the sig).
const SIGNED_FIELDS_LEN: usize =
    PUSH_GID_LEN + 1 + PUSH_WAKE_NONCE_LEN + 8 + PUSH_WAKE_KEY_LEN + PUSH_WAKE_KEY_LEN;

/// Length of a wake plaintext.
pub const PUSH_WAKE_PLAINTEXT_LEN: usize = SIGNED_FIELDS_LEN + SIGNATURE_LEN;

/// Length of a sealed wake: version, ephemeral key, plaintext, tag (234).
pub const PUSH_WAKE_LEN: usize = 1 + EPH_PUB_LEN + PUSH_WAKE_PLAINTEXT_LEN + TAG_LEN;

/// Length of a sealed wake as base64url without padding: the exact length of
/// the `x-lakko-wake` header value (312).
pub const PUSH_WAKE_HEADER_LEN: usize = PUSH_WAKE_LEN.div_ceil(3) * 4;

const _: () = assert!(PUSH_WAKE_PLAINTEXT_LEN == 185);
const _: () = assert!(PUSH_WAKE_LEN == 234);
const _: () = assert!(
    PUSH_WAKE_LEN % 3 == 0,
    "the header length has no padding term"
);
const _: () = assert!(PUSH_WAKE_HEADER_LEN == 312);

/// The HKDF labels of the wake seal (distinct from every other sealed box).
const PUSH_WAKE_SEAL_LABELS: SealLabels = SealLabels {
    key: b"lakko/push-wake/v1/key",
    nonce: b"lakko/push-wake/v1/nonce",
};

/// Subscription schema version.
pub const PUSH_SUBSCRIPTION_VERSION_V1: u8 = 0x01;

/// Domain separator that opens a subscription's AAD. Frozen.
pub const PUSH_SUBSCRIPTION_AAD_DOMAIN: &[u8] = b"lakko/push-subscription/v1\x00";

/// Length of a subscription id: random per (device, group), kept by the
/// device so it can remove its own row.
pub const PUSH_SUBSCRIPTION_ID_LEN: usize = 16;

/// Longest token one entry carries (push-gw's former column width).
pub const MAX_PUSH_TOKEN_LEN: usize = 2048;

/// Most tokens one subscription carries: one per provider.
pub const MAX_PUSH_TOKENS: usize = PushProvider::ALL.len();

/// A subscription plaintext is zero-padded to a multiple of this, so a row's
/// length at rest says little about which providers it holds. Chosen, not
/// measured.
pub const PUSH_SUBSCRIPTION_PAD_BLOCK: usize = 256;

/// What a subscription wire adds around its padded plaintext: version,
/// nonce, tag.
pub const PUSH_SUBSCRIPTION_OVERHEAD: usize = 1 + NONCE_LEN + TAG_LEN;

/// Longest padded subscription plaintext: every provider with a
/// longest token, rounded up to a pad block.
pub const MAX_PUSH_SUBSCRIPTION_PLAINTEXT_LEN: usize =
    (1 + MAX_PUSH_TOKENS * (1 + 2 + MAX_PUSH_TOKEN_LEN)).div_ceil(PUSH_SUBSCRIPTION_PAD_BLOCK)
        * PUSH_SUBSCRIPTION_PAD_BLOCK;

/// Shortest sealed subscription.
pub const MIN_PUSH_SUBSCRIPTION_LEN: usize =
    PUSH_SUBSCRIPTION_OVERHEAD + PUSH_SUBSCRIPTION_PAD_BLOCK;

/// Longest sealed subscription (push-gw's column must hold it).
pub const MAX_PUSH_SUBSCRIPTION_LEN: usize =
    PUSH_SUBSCRIPTION_OVERHEAD + MAX_PUSH_SUBSCRIPTION_PLAINTEXT_LEN;

const _: () = assert!(MAX_PUSH_SUBSCRIPTION_LEN == 10_525);

/// Why a wake or a subscription was refused. A reason code; never a key or
/// token byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PushWakeError {
    /// A wake or subscription has a length its format does not allow.
    #[error("push wire has a length its format does not allow")]
    BadLength,
    /// A wake or subscription carries an unknown schema version.
    #[error("unknown push wire version {0:#04x}")]
    UnknownVersion(u8),
    /// The wake did not open under push-gw's key, or the subscription did
    /// not open under the unlock key (another key, another row, tampered).
    #[error("push wire did not open")]
    NotOpened,
    /// The wake names a kind this version does not know.
    #[error("unknown wake kind {0:#04x}")]
    UnknownKind(u8),
    /// The wake key in a wake is not a usable Ed25519 public key.
    #[error("wake key is not a usable public key")]
    BadKey,
    /// The wake's signature does not verify under the wake key it names.
    #[error("wake signature does not verify")]
    BadSignature,
    /// A subscription's token list is malformed: no entry, too many, a
    /// repeated or unknown provider, an empty or over-long token, non-zero
    /// padding.
    #[error("push subscription token list is malformed")]
    BadTokens,
}

impl PushWakeError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::NotOpened => "notOpened",
            Self::UnknownKind(_) => "unknownKind",
            Self::BadKey => "badKey",
            Self::BadSignature => "badSignature",
            Self::BadTokens => "badTokens",
        }
    }
}

/// What a wake tells the woken devices about. The byte values are frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PushWakeKind {
    /// An ordinary message.
    Message,
    /// A call signal: push-gw raises urgency and the device rings.
    Call,
}

impl PushWakeKind {
    /// The frozen wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Message => 0x01,
            Self::Call => 0x02,
        }
    }

    /// Parse a wire byte, fail-closed.
    ///
    /// # Errors
    ///
    /// [`PushWakeError::UnknownKind`] for any other byte.
    pub const fn from_byte(byte: u8) -> CoreResult<Self, PushWakeError> {
        match byte {
            0x01 => Ok(Self::Message),
            0x02 => Ok(Self::Call),
            other => Err(PushWakeError::UnknownKind(other)),
        }
    }
}

/// push-gw's X25519 key pair: wakes are sealed to its public half. `Debug`
/// never prints the secret.
#[derive(Clone)]
pub struct PushGwKey {
    secret: X25519SecretKey,
}

impl fmt::Debug for PushGwKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushGwKey")
            .field("public", &self.public())
            .finish_non_exhaustive()
    }
}

impl PushGwKey {
    /// A fresh key pair from OS randomness.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            secret: X25519SecretKey::generate(),
        }
    }

    /// The key pair whose secret is `bytes`.
    #[must_use]
    pub fn from_secret_bytes(bytes: [u8; PUSH_GW_KEY_LEN]) -> Self {
        Self {
            secret: X25519SecretKey::from_bytes(bytes),
        }
    }

    /// The 32-byte secret. Secret: whoever holds it opens every wake.
    #[must_use]
    pub fn secret_bytes(&self) -> SecretBytes<PUSH_GW_KEY_LEN> {
        SecretBytes::new(self.secret.to_bytes())
    }

    /// The public key clients seal wakes to.
    #[must_use]
    pub fn public(&self) -> [u8; PUSH_GW_KEY_LEN] {
        self.secret.public_key().to_bytes()
    }
}

/// The AAD of a wake.
fn wake_aad() -> [u8; 20] {
    let mut out = [0u8; 20];
    out[..PUSH_WAKE_AAD_DOMAIN.len()].copy_from_slice(PUSH_WAKE_AAD_DOMAIN);
    out[PUSH_WAKE_AAD_DOMAIN.len()] = PUSH_WAKE_VERSION_V1;
    out
}

const _: () = assert!(PUSH_WAKE_AAD_DOMAIN.len() + 1 == 20);

/// The bytes the wake key signs: the domain, then every plaintext field
/// before the signature.
fn wake_signed_bytes(fields: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(PUSH_WAKE_SIG_DOMAIN.len() + fields.len());
    out.extend_from_slice(PUSH_WAKE_SIG_DOMAIN);
    out.extend_from_slice(fields);
    out
}

/// Seal a wake for the group whose routing secret is `push_routing_secret`
/// to push-gw's public key `push_gw_pk`. `timestamp_ms` is the caller's
/// clock (milliseconds since the Unix epoch); push-gw refuses a wake whose
/// timestamp is far from its own clock. A fresh random nonce and ephemeral
/// key per call, so two wakes are never equal.
///
/// # Errors
///
/// [`PushWakeError::NotOpened`] if the AEAD seal fails (not reachable with
/// valid inputs; surfaced rather than panicking).
pub fn seal_push_wake(
    push_gw_pk: &[u8; PUSH_GW_KEY_LEN],
    push_routing_secret: &[u8; PUSH_ROUTING_SECRET_LEN],
    kind: PushWakeKind,
    timestamp_ms: u64,
) -> CoreResult<Vec<u8>, PushWakeError> {
    let mut nonce = [0u8; PUSH_WAKE_NONCE_LEN];
    random::fill(&mut nonce);
    seal_push_wake_with_nonce(push_gw_pk, push_routing_secret, kind, timestamp_ms, &nonce)
}

/// [`seal_push_wake`] with a caller-chosen nonce (tests, which need two wakes
/// with the same nonce to exercise push-gw's replay refusal).
///
/// # Errors
///
/// As [`seal_push_wake`].
pub fn seal_push_wake_with_nonce(
    push_gw_pk: &[u8; PUSH_GW_KEY_LEN],
    push_routing_secret: &[u8; PUSH_ROUTING_SECRET_LEN],
    kind: PushWakeKind,
    timestamp_ms: u64,
    nonce: &[u8; PUSH_WAKE_NONCE_LEN],
) -> CoreResult<Vec<u8>, PushWakeError> {
    let wake_key =
        SigningKey::from_seed(derive_push_wake_signing_seed(push_routing_secret).as_bytes());
    seal_with_signer(
        push_gw_pk,
        &derive_push_gid(push_routing_secret),
        kind,
        timestamp_ms,
        nonce,
        derive_push_wake_unlock_key(push_routing_secret).as_bytes(),
        &wake_key,
    )
}

/// Seal a wake whose fields and signer are all chosen by the caller. The
/// production entry points derive them from a routing secret; tests use
/// this to build wakes signed by the wrong key.
#[doc(hidden)]
pub fn seal_with_signer(
    push_gw_pk: &[u8; PUSH_GW_KEY_LEN],
    push_gid: &[u8; PUSH_GID_LEN],
    kind: PushWakeKind,
    timestamp_ms: u64,
    nonce: &[u8; PUSH_WAKE_NONCE_LEN],
    unlock_key: &[u8; PUSH_WAKE_KEY_LEN],
    signer: &SigningKey,
) -> CoreResult<Vec<u8>, PushWakeError> {
    seal_with_claimed_key(
        push_gw_pk,
        push_gid,
        kind,
        timestamp_ms,
        nonce,
        unlock_key,
        &signer.verifying_key().to_bytes(),
        signer,
    )
}

/// As [`seal_with_signer`] but naming `claimed_wake_pk` in the plaintext
/// whatever key actually signs - the forgery shape a test must see refused.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn seal_with_claimed_key(
    push_gw_pk: &[u8; PUSH_GW_KEY_LEN],
    push_gid: &[u8; PUSH_GID_LEN],
    kind: PushWakeKind,
    timestamp_ms: u64,
    nonce: &[u8; PUSH_WAKE_NONCE_LEN],
    unlock_key: &[u8; PUSH_WAKE_KEY_LEN],
    claimed_wake_pk: &[u8; PUSH_WAKE_KEY_LEN],
    signer: &SigningKey,
) -> CoreResult<Vec<u8>, PushWakeError> {
    let mut plaintext = Vec::with_capacity(PUSH_WAKE_PLAINTEXT_LEN);
    plaintext.extend_from_slice(push_gid);
    plaintext.push(kind.to_byte());
    plaintext.extend_from_slice(nonce);
    plaintext.extend_from_slice(&timestamp_ms.to_be_bytes());
    plaintext.extend_from_slice(unlock_key);
    plaintext.extend_from_slice(claimed_wake_pk);
    let signature = signer.sign(&wake_signed_bytes(&plaintext)).to_bytes();
    plaintext.extend_from_slice(&signature);

    let recipient = X25519PublicKey::from_array(*push_gw_pk);
    let sealed =
        envelope::seal_labeled(&PUSH_WAKE_SEAL_LABELS, &recipient, &wake_aad(), &plaintext)
            .map_err(|_| PushWakeError::NotOpened)?;
    let mut out = Vec::with_capacity(PUSH_WAKE_LEN);
    out.push(PUSH_WAKE_VERSION_V1);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// A wake push-gw opened and whose signature verified under the wake key it
/// names. `Debug` never prints the unlock key.
#[derive(Clone)]
pub struct OpenedPushWake {
    /// The group's routing id.
    pub push_gid: [u8; PUSH_GID_LEN],
    /// Message or call.
    pub kind: PushWakeKind,
    /// What push-gw deduplicates on.
    pub nonce: [u8; PUSH_WAKE_NONCE_LEN],
    /// The sender's clock, milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
    /// The wake key the signature verified under: push-gw delivers only to
    /// subscriptions registered under this key.
    pub wake_public_key: [u8; PUSH_WAKE_KEY_LEN],
    unlock_key: SecretBytes<PUSH_WAKE_KEY_LEN>,
}

impl fmt::Debug for OpenedPushWake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenedPushWake")
            .field("kind", &self.kind)
            .field("timestamp_ms", &self.timestamp_ms)
            .finish_non_exhaustive()
    }
}

impl OpenedPushWake {
    /// The unlock key the subscriptions of this group are sealed under.
    /// Secret: push-gw must hold it only while it serves this wake.
    #[must_use]
    pub const fn unlock_key(&self) -> &SecretBytes<PUSH_WAKE_KEY_LEN> {
        &self.unlock_key
    }
}

/// Open a sealed wake with push-gw's key and verify its signature under the
/// wake key it names. Verifying the signature proves only that the sender
/// held that key's seed; push-gw must still deliver only to subscriptions
/// registered under [`OpenedPushWake::wake_public_key`].
///
/// # Errors
///
/// [`PushWakeError::BadLength`], [`PushWakeError::UnknownVersion`],
/// [`PushWakeError::NotOpened`], [`PushWakeError::UnknownKind`],
/// [`PushWakeError::BadKey`], [`PushWakeError::BadSignature`].
pub fn open_push_wake(key: &PushGwKey, wire: &[u8]) -> CoreResult<OpenedPushWake, PushWakeError> {
    if wire.len() != PUSH_WAKE_LEN {
        return Err(PushWakeError::BadLength);
    }
    if wire[0] != PUSH_WAKE_VERSION_V1 {
        return Err(PushWakeError::UnknownVersion(wire[0]));
    }
    let plaintext =
        envelope::open_labeled(&PUSH_WAKE_SEAL_LABELS, &key.secret, &wake_aad(), &wire[1..])
            .map_err(|_| PushWakeError::NotOpened)?;
    parse_wake_plaintext(&plaintext)
}

/// Parse and verify a wake plaintext (behind the AEAD, so reachable only by
/// a fuzzer through `fuzz_api`).
fn parse_wake_plaintext(plaintext: &[u8]) -> CoreResult<OpenedPushWake, PushWakeError> {
    if plaintext.len() != PUSH_WAKE_PLAINTEXT_LEN {
        return Err(PushWakeError::BadLength);
    }
    let (fields, signature) = plaintext.split_at(SIGNED_FIELDS_LEN);
    let mut at = 0usize;
    let mut take = |n: usize| {
        let part = &fields[at..at + n];
        at += n;
        part
    };
    let push_gid: [u8; PUSH_GID_LEN] = take(PUSH_GID_LEN)
        .try_into()
        .map_err(|_| PushWakeError::BadLength)?;
    let kind = PushWakeKind::from_byte(take(1)[0])?;
    let nonce: [u8; PUSH_WAKE_NONCE_LEN] = take(PUSH_WAKE_NONCE_LEN)
        .try_into()
        .map_err(|_| PushWakeError::BadLength)?;
    let timestamp: [u8; 8] = take(8).try_into().map_err(|_| PushWakeError::BadLength)?;
    let unlock: [u8; PUSH_WAKE_KEY_LEN] = take(PUSH_WAKE_KEY_LEN)
        .try_into()
        .map_err(|_| PushWakeError::BadLength)?;
    let wake_pk: [u8; PUSH_WAKE_KEY_LEN] = take(PUSH_WAKE_KEY_LEN)
        .try_into()
        .map_err(|_| PushWakeError::BadLength)?;

    let verifying = VerifyingKey::from_bytes(&wake_pk).map_err(|_| PushWakeError::BadKey)?;
    if verifying.is_weak() {
        return Err(PushWakeError::BadKey);
    }
    let signature = Signature::from_bytes(signature).map_err(|_| PushWakeError::BadSignature)?;
    verifying
        .verify(&wake_signed_bytes(fields), &signature)
        .map_err(|_| PushWakeError::BadSignature)?;

    Ok(OpenedPushWake {
        push_gid,
        kind,
        nonce,
        timestamp_ms: u64::from_be_bytes(timestamp),
        wake_public_key: wake_pk,
        unlock_key: SecretBytes::new(unlock),
    })
}

/// The push provider a token belongs to. The byte values are frozen and are
/// the ones push-gw has always used for its provider column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PushProvider {
    /// Apple Push Notification service, the alert token.
    Apns,
    /// Firebase Cloud Messaging.
    Fcm,
    /// Web Push (RFC 8030 + RFC 8291).
    WebPush,
    /// UnifiedPush.
    UnifiedPush,
    /// Apple PushKit, the call token of an iOS device.
    ApnsVoip,
}

impl PushProvider {
    /// Every provider, in wire-byte order.
    pub const ALL: [Self; 5] = [
        Self::Apns,
        Self::Fcm,
        Self::WebPush,
        Self::UnifiedPush,
        Self::ApnsVoip,
    ];

    /// The frozen wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Apns => 1,
            Self::Fcm => 2,
            Self::WebPush => 3,
            Self::UnifiedPush => 4,
            Self::ApnsVoip => 5,
        }
    }

    /// Parse a wire byte, fail-closed.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Apns),
            2 => Some(Self::Fcm),
            3 => Some(Self::WebPush),
            4 => Some(Self::UnifiedPush),
            5 => Some(Self::ApnsVoip),
            _ => None,
        }
    }

    /// The snake_case name the clients and push-gw spell a provider with.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Apns => "apns",
            Self::Fcm => "fcm",
            Self::WebPush => "web_push",
            Self::UnifiedPush => "unified_push",
            Self::ApnsVoip => "apns_voip",
        }
    }

    /// Parse a [`Self::wire_name`].
    #[must_use]
    pub fn from_wire_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.wire_name() == name)
    }
}

/// One provider token inside a subscription. `Debug` prints the provider and
/// the length only.
#[derive(Clone, PartialEq, Eq)]
pub struct PushTokenEntry {
    /// Which provider the token is for.
    pub provider: PushProvider,
    /// The provider-opaque token bytes.
    pub token: Vec<u8>,
}

impl fmt::Debug for PushTokenEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushTokenEntry")
            .field("provider", &self.provider)
            .field("token_len", &self.token.len())
            .finish()
    }
}

/// The AAD of a subscription.
fn subscription_aad(
    push_gid: &[u8; PUSH_GID_LEN],
    sub_id: &[u8; PUSH_SUBSCRIPTION_ID_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        PUSH_SUBSCRIPTION_AAD_DOMAIN.len() + 1 + PUSH_GID_LEN + PUSH_SUBSCRIPTION_ID_LEN,
    );
    out.extend_from_slice(PUSH_SUBSCRIPTION_AAD_DOMAIN);
    out.push(PUSH_SUBSCRIPTION_VERSION_V1);
    out.extend_from_slice(push_gid);
    out.extend_from_slice(sub_id);
    out
}

fn check_entries(entries: &[PushTokenEntry]) -> CoreResult<(), PushWakeError> {
    if entries.is_empty() || entries.len() > MAX_PUSH_TOKENS {
        return Err(PushWakeError::BadTokens);
    }
    for (i, entry) in entries.iter().enumerate() {
        if entry.token.is_empty() || entry.token.len() > MAX_PUSH_TOKEN_LEN {
            return Err(PushWakeError::BadTokens);
        }
        if entries[..i].iter().any(|e| e.provider == entry.provider) {
            return Err(PushWakeError::BadTokens);
        }
    }
    Ok(())
}

/// Seal `entries` (a device's provider tokens) as the subscription row
/// `sub_id` of the group `push_gid`, under that group's unlock key.
///
/// # Errors
///
/// [`PushWakeError::BadTokens`] for no entry, more than
/// [`MAX_PUSH_TOKENS`], a repeated provider, or an empty or over-long token;
/// [`PushWakeError::NotOpened`] if the AEAD seal fails (not reachable).
pub fn seal_push_subscription(
    unlock_key: &[u8; PUSH_WAKE_KEY_LEN],
    push_gid: &[u8; PUSH_GID_LEN],
    sub_id: &[u8; PUSH_SUBSCRIPTION_ID_LEN],
    entries: &[PushTokenEntry],
) -> CoreResult<Vec<u8>, PushWakeError> {
    check_entries(entries)?;
    let mut plaintext = Vec::new();
    #[allow(clippy::cast_possible_truncation)] // bounded by MAX_PUSH_TOKENS
    plaintext.push(entries.len() as u8);
    for entry in entries {
        plaintext.push(entry.provider.to_byte());
        #[allow(clippy::cast_possible_truncation)] // bounded by MAX_PUSH_TOKEN_LEN
        plaintext.extend_from_slice(&(entry.token.len() as u16).to_be_bytes());
        plaintext.extend_from_slice(&entry.token);
    }
    let padded =
        plaintext.len().div_ceil(PUSH_SUBSCRIPTION_PAD_BLOCK).max(1) * PUSH_SUBSCRIPTION_PAD_BLOCK;
    plaintext.resize(padded, 0);

    let mut nonce: Nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let ciphertext = aead::seal(
        Algorithm::ChaCha20Poly1305,
        &AeadKey::new(*unlock_key),
        &nonce,
        &subscription_aad(push_gid, sub_id),
        &plaintext,
    )
    .map_err(|_| PushWakeError::NotOpened)?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(PUSH_SUBSCRIPTION_VERSION_V1);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// [`seal_push_subscription`] for the group whose routing secret is
/// `push_routing_secret`: the client entry point, which derives the
/// `push_gid` and the unlock key so neither crosses an FFI boundary.
///
/// # Errors
///
/// As [`seal_push_subscription`].
pub fn seal_push_subscription_for_secret(
    push_routing_secret: &[u8; PUSH_ROUTING_SECRET_LEN],
    sub_id: &[u8; PUSH_SUBSCRIPTION_ID_LEN],
    entries: &[PushTokenEntry],
) -> CoreResult<Vec<u8>, PushWakeError> {
    seal_push_subscription(
        derive_push_wake_unlock_key(push_routing_secret).as_bytes(),
        &derive_push_gid(push_routing_secret),
        sub_id,
        entries,
    )
}

/// Whether `len` is a length a sealed subscription can have. push-gw checks
/// it at registration, before storing a row it could never open.
#[must_use]
pub const fn is_well_formed_subscription_len(len: usize) -> bool {
    len >= MIN_PUSH_SUBSCRIPTION_LEN
        && len <= MAX_PUSH_SUBSCRIPTION_LEN
        && (len - PUSH_SUBSCRIPTION_OVERHEAD) % PUSH_SUBSCRIPTION_PAD_BLOCK == 0
}

/// Open the subscription row `sub_id` of the group `push_gid` under the
/// unlock key a wake delivered.
///
/// # Errors
///
/// [`PushWakeError::BadLength`], [`PushWakeError::UnknownVersion`],
/// [`PushWakeError::NotOpened`], [`PushWakeError::BadTokens`].
pub fn open_push_subscription(
    unlock_key: &[u8; PUSH_WAKE_KEY_LEN],
    push_gid: &[u8; PUSH_GID_LEN],
    sub_id: &[u8; PUSH_SUBSCRIPTION_ID_LEN],
    wire: &[u8],
) -> CoreResult<Vec<PushTokenEntry>, PushWakeError> {
    if !is_well_formed_subscription_len(wire.len()) {
        return Err(PushWakeError::BadLength);
    }
    if wire[0] != PUSH_SUBSCRIPTION_VERSION_V1 {
        return Err(PushWakeError::UnknownVersion(wire[0]));
    }
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&wire[1..=NONCE_LEN]);
    let plaintext = aead::open(
        Algorithm::ChaCha20Poly1305,
        &AeadKey::new(*unlock_key),
        &nonce,
        &subscription_aad(push_gid, sub_id),
        &wire[1 + NONCE_LEN..],
    )
    .map_err(|_| PushWakeError::NotOpened)?;
    parse_subscription_plaintext(&plaintext)
}

/// Parse a subscription plaintext (behind the AEAD).
fn parse_subscription_plaintext(
    plaintext: &[u8],
) -> CoreResult<Vec<PushTokenEntry>, PushWakeError> {
    let (&count, mut rest) = plaintext.split_first().ok_or(PushWakeError::BadTokens)?;
    let count = usize::from(count);
    if count == 0 || count > MAX_PUSH_TOKENS {
        return Err(PushWakeError::BadTokens);
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if rest.len() < 3 {
            return Err(PushWakeError::BadTokens);
        }
        let provider = PushProvider::from_byte(rest[0]).ok_or(PushWakeError::BadTokens)?;
        let len = usize::from(u16::from_be_bytes([rest[1], rest[2]]));
        rest = &rest[3..];
        if len == 0 || len > MAX_PUSH_TOKEN_LEN || rest.len() < len {
            return Err(PushWakeError::BadTokens);
        }
        entries.push(PushTokenEntry {
            provider,
            token: rest[..len].to_vec(),
        });
        rest = &rest[len..];
    }
    if rest.iter().any(|b| *b != 0) {
        return Err(PushWakeError::BadTokens);
    }
    check_entries(&entries)?;
    Ok(entries)
}

/// The fuzz oracle behind `fuzz_api::push_wake_open`: open `wire` under a
/// fixed push-gw key. `None` when the open refuses. A fuzzer cannot pass the
/// AEAD, so this exercises the length, version and AEAD refusal; anything
/// that does open must carry a signature that verified.
pub(crate) fn fuzz_open_wake(wire: &[u8]) -> Option<bool> {
    let key = PushGwKey::from_secret_bytes([0x5A; PUSH_GW_KEY_LEN]);
    let opened = open_push_wake(&key, wire).ok()?;
    Some(opened.kind == PushWakeKind::Message || opened.kind == PushWakeKind::Call)
}

/// The fuzz oracle behind `fuzz_api::push_wake_plaintext`: parse and verify
/// a wake plaintext directly. `None` when it refuses.
pub(crate) fn fuzz_wake_plaintext(plaintext: &[u8]) -> Option<bool> {
    let opened = parse_wake_plaintext(plaintext).ok()?;
    Some(plaintext[..PUSH_GID_LEN] == opened.push_gid)
}

/// The fuzz oracle behind `fuzz_api::push_subscription_plaintext`: parse a
/// subscription plaintext directly, and re-seal what parsed. `None` when it
/// refuses; `Some(true)` when the re-sealed row opens to the same entries.
pub(crate) fn fuzz_subscription_plaintext(plaintext: &[u8]) -> Option<bool> {
    let entries = parse_subscription_plaintext(plaintext).ok()?;
    let key = [0x5B; PUSH_WAKE_KEY_LEN];
    let gid = [0x5C; PUSH_GID_LEN];
    let sub = [0x5D; PUSH_SUBSCRIPTION_ID_LEN];
    let sealed = seal_push_subscription(&key, &gid, &sub, &entries).ok()?;
    Some(open_push_subscription(&key, &gid, &sub, &sealed).ok() == Some(entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push_routing::derive_push_wake_public_key;

    const SECRET: [u8; 32] = [0x42; 32];

    fn entry(provider: PushProvider, byte: u8, len: usize) -> PushTokenEntry {
        PushTokenEntry {
            provider,
            token: vec![byte; len],
        }
    }

    #[test]
    fn a_wake_seals_and_opens_with_every_field() {
        let gw = PushGwKey::generate();
        let wire =
            seal_push_wake(&gw.public(), &SECRET, PushWakeKind::Call, 1_700_000_000_123).unwrap();
        assert_eq!(wire.len(), PUSH_WAKE_LEN);
        let opened = open_push_wake(&gw, &wire).unwrap();
        assert_eq!(opened.push_gid, derive_push_gid(&SECRET));
        assert_eq!(opened.kind, PushWakeKind::Call);
        assert_eq!(opened.timestamp_ms, 1_700_000_000_123);
        assert_eq!(opened.wake_public_key, derive_push_wake_public_key(&SECRET));
        assert_eq!(
            opened.unlock_key().as_bytes(),
            derive_push_wake_unlock_key(&SECRET).as_bytes()
        );
    }

    #[test]
    fn two_wakes_are_never_equal_and_carry_distinct_nonces() {
        let gw = PushGwKey::generate();
        let a = seal_push_wake(&gw.public(), &SECRET, PushWakeKind::Message, 1).unwrap();
        let b = seal_push_wake(&gw.public(), &SECRET, PushWakeKind::Message, 1).unwrap();
        assert_ne!(a, b);
        assert_ne!(
            open_push_wake(&gw, &a).unwrap().nonce,
            open_push_wake(&gw, &b).unwrap().nonce
        );
    }

    #[test]
    fn a_wake_for_another_push_gw_does_not_open() {
        let gw = PushGwKey::generate();
        let other = PushGwKey::generate();
        let wire = seal_push_wake(&other.public(), &SECRET, PushWakeKind::Message, 1).unwrap();
        assert_eq!(
            open_push_wake(&gw, &wire).unwrap_err(),
            PushWakeError::NotOpened
        );
    }

    #[test]
    fn a_wake_signed_by_another_key_than_it_names_is_refused() {
        // The forgery the ADR names: somebody without the routing secret
        // claims the group's wake key but cannot sign under it.
        let gw = PushGwKey::generate();
        let attacker = SigningKey::from_seed(&[0x99; 32]);
        let wire = seal_with_claimed_key(
            &gw.public(),
            &derive_push_gid(&SECRET),
            PushWakeKind::Message,
            1,
            &[1; PUSH_WAKE_NONCE_LEN],
            &[2; 32],
            &derive_push_wake_public_key(&SECRET),
            &attacker,
        )
        .unwrap();
        assert_eq!(
            open_push_wake(&gw, &wire).unwrap_err(),
            PushWakeError::BadSignature
        );
    }

    #[test]
    fn a_tampered_or_mis_sized_wake_is_refused() {
        let gw = PushGwKey::generate();
        let wire = seal_push_wake(&gw.public(), &SECRET, PushWakeKind::Message, 1).unwrap();
        for i in [0usize, 1, 40, PUSH_WAKE_LEN - 1] {
            let mut bad = wire.clone();
            bad[i] ^= 0x01;
            assert!(open_push_wake(&gw, &bad).is_err(), "flip at {i} accepted");
        }
        assert_eq!(
            open_push_wake(&gw, &wire[..PUSH_WAKE_LEN - 1]).unwrap_err(),
            PushWakeError::BadLength
        );
        let mut longer = wire;
        longer.push(0);
        assert_eq!(
            open_push_wake(&gw, &longer).unwrap_err(),
            PushWakeError::BadLength
        );
        assert_eq!(
            open_push_wake(&gw, &[]).unwrap_err(),
            PushWakeError::BadLength
        );
    }

    #[test]
    fn an_unknown_wake_version_is_refused_before_opening() {
        let gw = PushGwKey::generate();
        let mut wire = seal_push_wake(&gw.public(), &SECRET, PushWakeKind::Message, 1).unwrap();
        wire[0] = 0x02;
        assert_eq!(
            open_push_wake(&gw, &wire).unwrap_err(),
            PushWakeError::UnknownVersion(0x02)
        );
    }

    #[test]
    fn an_unknown_wake_kind_is_refused() {
        // Built through the plaintext parser: a valid signature over a kind
        // byte nobody may send.
        let signer = SigningKey::from_seed(derive_push_wake_signing_seed(&SECRET).as_bytes());
        let mut fields = Vec::new();
        fields.extend_from_slice(&derive_push_gid(&SECRET));
        fields.push(0x07);
        fields.extend_from_slice(&[0u8; PUSH_WAKE_NONCE_LEN]);
        fields.extend_from_slice(&1u64.to_be_bytes());
        fields.extend_from_slice(&[0u8; 32]);
        fields.extend_from_slice(&signer.verifying_key().to_bytes());
        let sig = signer.sign(&wake_signed_bytes(&fields)).to_bytes();
        fields.extend_from_slice(&sig);
        assert_eq!(
            parse_wake_plaintext(&fields).unwrap_err(),
            PushWakeError::UnknownKind(0x07)
        );
    }

    #[test]
    fn the_wake_kind_bytes_are_frozen() {
        assert_eq!(PushWakeKind::Message.to_byte(), 0x01);
        assert_eq!(PushWakeKind::Call.to_byte(), 0x02);
        assert_eq!(
            PushWakeKind::from_byte(0x00),
            Err(PushWakeError::UnknownKind(0))
        );
    }

    #[test]
    fn the_header_length_is_the_base64url_length_of_a_wake() {
        assert_eq!(PUSH_WAKE_HEADER_LEN, 312);
    }

    #[test]
    fn the_seal_labels_are_not_the_mailbox_ones() {
        // A wake sealed to an X25519 key must not open as anything else
        // sealed to the same key with the same AAD: the labels separate.
        let recipient = X25519SecretKey::generate();
        let pt = [0u8; 8];
        let wake = envelope::seal_labeled(
            &PUSH_WAKE_SEAL_LABELS,
            &recipient.public_key(),
            &wake_aad(),
            &pt,
        )
        .unwrap();
        assert!(envelope::open(&recipient, &wake_aad(), &wake).is_err());
    }

    #[test]
    fn a_subscription_seals_and_opens() {
        let gid = derive_push_gid(&SECRET);
        let sub = [7u8; PUSH_SUBSCRIPTION_ID_LEN];
        let entries = vec![
            entry(PushProvider::Apns, 1, 32),
            entry(PushProvider::ApnsVoip, 2, 32),
        ];
        let wire = seal_push_subscription_for_secret(&SECRET, &sub, &entries).unwrap();
        assert!(is_well_formed_subscription_len(wire.len()));
        assert_eq!(wire.len(), MIN_PUSH_SUBSCRIPTION_LEN);
        let opened = open_push_subscription(
            derive_push_wake_unlock_key(&SECRET).as_bytes(),
            &gid,
            &sub,
            &wire,
        )
        .unwrap();
        assert_eq!(opened, entries);
    }

    #[test]
    fn a_subscription_holds_no_token_in_the_clear_and_differs_per_seal() {
        let sub = [7u8; PUSH_SUBSCRIPTION_ID_LEN];
        let token = b"a-very-recognisable-push-token".to_vec();
        let entries = vec![PushTokenEntry {
            provider: PushProvider::Fcm,
            token: token.clone(),
        }];
        let a = seal_push_subscription_for_secret(&SECRET, &sub, &entries).unwrap();
        let b = seal_push_subscription_for_secret(&SECRET, &sub, &entries).unwrap();
        let other_group = seal_push_subscription_for_secret(&[0x43; 32], &sub, &entries).unwrap();
        assert_ne!(a, b, "a fresh nonce per seal");
        assert_ne!(a, other_group);
        for wire in [&a, &b, &other_group] {
            assert!(!wire.windows(token.len()).any(|w| w == token.as_slice()));
        }
    }

    #[test]
    fn a_subscription_opens_only_as_its_own_row_of_its_own_group() {
        let gid = derive_push_gid(&SECRET);
        let unlock = derive_push_wake_unlock_key(&SECRET);
        let sub = [7u8; PUSH_SUBSCRIPTION_ID_LEN];
        let entries = vec![entry(PushProvider::WebPush, 3, 500)];
        let wire = seal_push_subscription_for_secret(&SECRET, &sub, &entries).unwrap();
        let wrong_sub = [8u8; PUSH_SUBSCRIPTION_ID_LEN];
        let wrong_gid = [9u8; PUSH_GID_LEN];
        let wrong_key = [1u8; 32];
        assert_eq!(
            open_push_subscription(unlock.as_bytes(), &gid, &wrong_sub, &wire),
            Err(PushWakeError::NotOpened)
        );
        assert_eq!(
            open_push_subscription(unlock.as_bytes(), &wrong_gid, &sub, &wire),
            Err(PushWakeError::NotOpened)
        );
        assert_eq!(
            open_push_subscription(&wrong_key, &gid, &sub, &wire),
            Err(PushWakeError::NotOpened)
        );
    }

    #[test]
    fn bad_token_lists_are_refused_at_seal() {
        let sub = [7u8; PUSH_SUBSCRIPTION_ID_LEN];
        let cases: Vec<Vec<PushTokenEntry>> = vec![
            vec![],
            vec![entry(PushProvider::Fcm, 1, 0)],
            vec![entry(PushProvider::Fcm, 1, MAX_PUSH_TOKEN_LEN + 1)],
            vec![
                entry(PushProvider::Fcm, 1, 4),
                entry(PushProvider::Fcm, 2, 4),
            ],
        ];
        for entries in cases {
            assert_eq!(
                seal_push_subscription_for_secret(&SECRET, &sub, &entries),
                Err(PushWakeError::BadTokens)
            );
        }
    }

    #[test]
    fn the_longest_subscription_fits_its_bound() {
        let sub = [7u8; PUSH_SUBSCRIPTION_ID_LEN];
        let entries: Vec<PushTokenEntry> = PushProvider::ALL
            .into_iter()
            .map(|p| entry(p, 0xAA, MAX_PUSH_TOKEN_LEN))
            .collect();
        let wire = seal_push_subscription_for_secret(&SECRET, &sub, &entries).unwrap();
        assert_eq!(wire.len(), MAX_PUSH_SUBSCRIPTION_LEN);
    }

    #[test]
    fn non_zero_padding_or_a_bad_count_is_refused_by_the_parser() {
        let mut plain = vec![1u8, 2, 0, 1, 0xAA];
        plain.resize(PUSH_SUBSCRIPTION_PAD_BLOCK, 0);
        assert!(parse_subscription_plaintext(&plain).is_ok());
        let mut dirty = plain.clone();
        dirty[100] = 1;
        assert_eq!(
            parse_subscription_plaintext(&dirty),
            Err(PushWakeError::BadTokens)
        );
        let mut zero = plain.clone();
        zero[0] = 0;
        assert_eq!(
            parse_subscription_plaintext(&zero),
            Err(PushWakeError::BadTokens)
        );
        let mut unknown = plain;
        unknown[1] = 9;
        assert_eq!(
            parse_subscription_plaintext(&unknown),
            Err(PushWakeError::BadTokens)
        );
    }

    #[test]
    fn provider_bytes_and_names_are_frozen() {
        let expected = [
            (PushProvider::Apns, 1, "apns"),
            (PushProvider::Fcm, 2, "fcm"),
            (PushProvider::WebPush, 3, "web_push"),
            (PushProvider::UnifiedPush, 4, "unified_push"),
            (PushProvider::ApnsVoip, 5, "apns_voip"),
        ];
        for (p, b, n) in expected {
            assert_eq!(p.to_byte(), b);
            assert_eq!(PushProvider::from_byte(b), Some(p));
            assert_eq!(p.wire_name(), n);
            assert_eq!(PushProvider::from_wire_name(n), Some(p));
        }
        assert_eq!(PushProvider::from_byte(0), None);
        assert_eq!(PushProvider::from_byte(6), None);
        assert_eq!(PushProvider::from_wire_name("voip"), None);
    }

    #[test]
    fn the_fuzz_oracles_hold_on_genuine_input() {
        let gw = PushGwKey::from_secret_bytes([0x5A; PUSH_GW_KEY_LEN]);
        let wire = seal_push_wake(&gw.public(), &SECRET, PushWakeKind::Message, 1).unwrap();
        assert_eq!(fuzz_open_wake(&wire), Some(true));
        let mut plain = vec![1u8, 2, 0, 1, 0xAA];
        plain.resize(PUSH_SUBSCRIPTION_PAD_BLOCK, 0);
        assert_eq!(fuzz_subscription_plaintext(&plain), Some(true));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn any_token_list_round_trips(
                providers in proptest::sample::subsequence(PushProvider::ALL.to_vec(), 1..=5),
                lens in proptest::collection::vec(1usize..=MAX_PUSH_TOKEN_LEN, 5),
                sub in any::<[u8; PUSH_SUBSCRIPTION_ID_LEN]>(),
                secret in any::<[u8; 32]>(),
            ) {
                let entries: Vec<PushTokenEntry> = providers
                    .iter()
                    .zip(lens.iter())
                    .map(|(p, l)| entry(*p, p.to_byte(), *l))
                    .collect();
                let wire = seal_push_subscription_for_secret(&secret, &sub, &entries).unwrap();
                prop_assert!(is_well_formed_subscription_len(wire.len()));
                let opened = open_push_subscription(
                    derive_push_wake_unlock_key(&secret).as_bytes(),
                    &derive_push_gid(&secret),
                    &sub,
                    &wire,
                ).unwrap();
                prop_assert_eq!(opened, entries);
            }

            #[test]
            fn arbitrary_bytes_never_open_as_a_wake(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
                let gw = PushGwKey::from_secret_bytes([0x5A; PUSH_GW_KEY_LEN]);
                prop_assert!(open_push_wake(&gw, &bytes).is_err());
            }

            #[test]
            fn arbitrary_plaintexts_never_panic_the_parsers(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
                let _ = parse_wake_plaintext(&bytes);
                let _ = parse_subscription_plaintext(&bytes);
            }
        }
    }
}
