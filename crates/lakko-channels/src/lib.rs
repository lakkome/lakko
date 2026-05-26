// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel broadcast (Phase 4a — publisher path).
//!
//! Implements the cryptographic substrate described in
//! [`ADR-0010`](../../../docs/adr/0010-channels-two-tier-broadcast.md):
//! **two-tier** architecture where a small PublisherGroup (vanilla
//! MLS, ≤ 500 members) holds the rotation authority for a 32-byte
//! `content_key_seed`, and subscribers receive HPKE-sealed envelopes
//! carrying that seed.
//!
//! ## What's here (4a)
//!
//! - [`seed`] — derives the channel content-key seed from a
//!   PublisherGroup's MLS exporter. The caller (a publisher who
//!   already owns an MLS [`Group`]) hands us the 32 raw bytes that
//!   came out of `mls_exporter("lakko/channel/v1", channel_id, 32)`;
//!   we wrap them in a [`ContentKeySeed`] handle and forbid copying.
//! - [`post`] — composes and verifies a [`ChannelPost`]: per-message
//!   AEAD key + nonce HKDF off the seed, ChaCha20-Poly1305 over
//!   plaintext, Ed25519 signature over
//!   `channel_id || epoch_id || sender_pubkey || message_counter || ciphertext`.
//! - [`epoch`] — monotone `EpochId` counter + serialisation helpers,
//!   advances on every PublisherGroup commit and on scheduled
//!   rotations.
//!
//! ## What's also here (4b)
//!
//! - [`subscribe`] — HPKE-equivalent seed envelope:
//!   [`seal_seed`] / [`open_seed`] + [`SEED_ENVELOPE_LEN`].
//!   Wire shape: `eph_pub(32) || sealed_seed(32) || tag(16) = 80`.
//!   AAD binds `channel_id || epoch_id` so a misrouted envelope
//!   to the wrong channel or wrong epoch fails the AEAD
//!   authenticator. Exposed across UniFFI / wasm-bindgen as
//!   `ChannelSubscriberKeypair` + `sealSubscriberSeedEnvelope` +
//!   `openSubscriberSeedEnvelope` (sprint G4-3c).
//!
//! ## What's NOT here yet
//!
//! - **Rotation distribution wire (4c).** The fanout
//!   service + batch envelope generator are over in
//!   `services/channels` (already shipped — see
//!   `POST /v1/channels/:id/rotate` and `docs/security/calls.md`
//!   C-42). The client-side rotation loop that walks the
//!   subscriber registry and seals N envelopes is a follow-up
//!   wave.
//! - **`@channel` discovery.** Lives in `services/discovery` (OPRF
//!   lookup) — already shipped via G4-5.
//!
//! ## Threat-model notes (cross-link to ADR-0010)
//!
//! - The seed is the **only** secret that controls channel
//!   confidentiality. We keep it inside `Zeroizing<[u8; 32]>` and
//!   never expose the raw bytes through the public API — callers
//!   pass the [`ContentKeySeed`] handle by reference.
//! - The Ed25519 signature is what lets a subscriber distinguish
//!   posts from different publishers even though they share the
//!   same seed. A subscriber who learned the seed (legitimately or
//!   via compromise) still cannot forge a post by another publisher
//!   without that publisher's identity Ed25519 private key.
//! - HKDF binds the per-message key to `(channel_id, epoch_id,
//!   sender_pubkey)` and binds the per-message nonce to
//!   `(channel_id, epoch_id, sender_pubkey, message_counter)`. Two
//!   publishers using the same seed therefore never collide on
//!   keys; two posts from one publisher with the same counter
//!   would collide on key + nonce — the `message_counter` invariant
//!   is enforced client-side and surfaced as a hard error in
//!   `verify_and_decrypt` if a duplicate is observed.
//! - AAD on the AEAD seal is the same byte string the Ed25519
//!   signature commits to, so a subscriber who fails to validate
//!   the signature short-circuits without ever asking the AEAD to
//!   decrypt — defence in depth against ciphertext-malleability
//!   experiments.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod epoch;
pub mod error;
pub mod post;
pub mod seed;
pub mod subscribe;

pub use epoch::{EpochId, EPOCH_ID_LEN};
pub use error::{ChannelError, Result};
pub use post::{
    compose_post, decode_post, verify_and_decrypt, ChannelPost, MAX_PLAINTEXT_LEN,
    POST_SIG_DOMAIN_V1,
};
pub use seed::{ContentKeySeed, CONTENT_KEY_SEED_LEN, EXPORTER_LABEL_V1};
pub use subscribe::{open_seed, seal_seed, EPH_PUB_LEN, SEED_ENVELOPE_LEN, TAG_LEN};

/// Channel identifier length — 16 bytes (the pseudonym chosen at
/// channel creation, per ADR-0010 § "Server can correlate" mitigation).
pub const CHANNEL_ID_LEN: usize = 16;
