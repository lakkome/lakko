// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Read-key envelope ([ADR-0087] section 6, phase B note of 2026-11-05): a
//! channel's owner hands one co-publisher the channel's 32-byte **read key**,
//! so any publisher's device can file the public seed of a public channel's
//! next epoch (`lakko_channels::seal_public_seed`).
//!
//! ## Why a carrier is needed
//!
//! The read key is `BLAKE3-derive-key("lakko/channels/read-key/v1",
//! owner_account_seed ‖ channel_id)` (`lakko_channels::channel_read_key`):
//! every device of the owner derives it, a co-publisher cannot. Reading it
//! back from the channel's public record needs a lookup per channel and only
//! works while the channel is public; a co-publisher of a private channel
//! must hold it the moment the owner makes the channel public. So the owner
//! seals it to each co-publisher's account key once, beside the admission
//! seed it already sends (`crate::admission_seed`), on the same
//! PublisherGroup journal. It carries no version: the key never changes for a
//! channel.
//!
//! ## Wire format (v1)
//!
//! ```text
//! CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1 ("lakko/channel-read-key/v1\0")
//!   ‖ version(1)=0x01 ‖ channel_id(16) ‖ member_account_key(32) ‖ sealed(80)
//! sealed = eph_pub(32) ‖ ChaCha20-Poly1305(read_key(32)) ‖ tag(16),
//!          AAD = every byte before it
//! ```
//!
//! Fixed length [`CHANNEL_READ_KEY_ENVELOPE_LEN`] (155). The sealed box is
//! [`crate::envelope`]'s, the shape of [`crate::admission_seed`] without a
//! public key (a read key derives none).
//!
//! ## The receiver's authority rule
//!
//! A device keeps the key only when all of these hold, judged at receipt
//! (C-68):
//!
//! 1. it arrived on the PublisherGroup journal of the channel `channel_id`
//!    names, and this device publishes that channel;
//! 2. `member_account_key` is this device's own account key;
//! 3. the sender is the channel's owner (the issuer of the co-publisher link
//!    this device joined by; a PublisherGroup has no role table);
//! 4. the box opens.
//!
//! Anything else is dropped. A wrong key from anyone else at worst seals a
//! public seed nobody opens: subscribers open public seeds with the key the
//! channel's public record carries.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use core::result::Result as CoreResult;

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use thiserror::Error;

use crate::envelope::{self, EPH_PUB_LEN, TAG_LEN};

/// 26-byte domain separator prepended to the envelope. Frozen: the
/// dispatcher matches on it.
pub const CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/channel-read-key/v1\x00";

/// Schema-version byte carried first after the separator.
pub const CHANNEL_READ_KEY_VERSION_V1: u8 = 0x01;

/// Length of a read key.
pub const CHANNEL_READ_KEY_LEN: usize = 32;

/// Length of a channel id.
pub const CHANNEL_READ_KEY_CHANNEL_ID_LEN: usize = 16;

/// The sealed key: ephemeral key, ciphertext of the 32-byte key, tag.
pub const CHANNEL_READ_KEY_SEALED_LEN: usize = EPH_PUB_LEN + CHANNEL_READ_KEY_LEN + TAG_LEN;

/// Bytes after the separator: version, channel id, account key, sealed.
pub const CHANNEL_READ_KEY_PAYLOAD_LEN: usize =
    1 + CHANNEL_READ_KEY_CHANNEL_ID_LEN + 32 + CHANNEL_READ_KEY_SEALED_LEN;

/// The one well-formed length.
pub const CHANNEL_READ_KEY_ENVELOPE_LEN: usize =
    CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1.len() + CHANNEL_READ_KEY_PAYLOAD_LEN;

const CHANNEL_AT: usize = 1;
const MEMBER_AT: usize = CHANNEL_AT + CHANNEL_READ_KEY_CHANNEL_ID_LEN;
const SEALED_AT: usize = MEMBER_AT + 32;

/// A decoded read-key envelope. The key is still sealed: [`Self::open`] with
/// the addressee's account X25519 secret recovers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelReadKeyEnvelope {
    /// The channel the read key belongs to.
    pub channel_id: [u8; CHANNEL_READ_KEY_CHANNEL_ID_LEN],
    /// The addressee's account Ed25519 public key.
    pub member_account_key: [u8; 32],
    /// The sealed 32-byte read key.
    pub sealed: [u8; CHANNEL_READ_KEY_SEALED_LEN],
}

/// Why a read-key envelope was refused. A reason code; nothing from the
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelReadKeyError {
    /// No domain separator. Only from [`decode_channel_read_key`].
    #[error("plaintext does not carry the channel read-key domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("channel read-key envelope has the wrong length")]
    BadLength,
    /// The version byte is not [`CHANNEL_READ_KEY_VERSION_V1`].
    #[error("channel read-key envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// Sealing the key failed.
    #[error("channel read key could not be sealed")]
    Seal,
    /// The sealed key did not open under the given key.
    #[error("channel read key did not open")]
    Open,
}

impl ChannelReadKeyError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::Seal => "seal",
            Self::Open => "open",
        }
    }
}

fn header(
    channel_id: &[u8; CHANNEL_READ_KEY_CHANNEL_ID_LEN],
    member_account_key: &[u8; 32],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHANNEL_READ_KEY_ENVELOPE_LEN);
    out.extend_from_slice(CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1);
    out.push(CHANNEL_READ_KEY_VERSION_V1);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(member_account_key);
    out
}

/// Compose the envelope handed to the MLS application-message send path on
/// the channel's PublisherGroup journal. `member_x25519_pub` is the
/// co-publisher account's X25519 public key (its identity bundle's
/// `x25519_pubkey`).
///
/// # Errors
///
/// [`ChannelReadKeyError::Seal`] if the sealed box fails (not expected).
pub fn compose_channel_read_key(
    channel_id: &[u8; CHANNEL_READ_KEY_CHANNEL_ID_LEN],
    member_account_key: &[u8; 32],
    member_x25519_pub: &X25519PublicKey,
    read_key: &[u8; CHANNEL_READ_KEY_LEN],
) -> CoreResult<Vec<u8>, ChannelReadKeyError> {
    let mut out = header(channel_id, member_account_key);
    let sealed =
        envelope::seal(member_x25519_pub, &out, read_key).map_err(|_| ChannelReadKeyError::Seal)?;
    if sealed.len() != CHANNEL_READ_KEY_SEALED_LEN {
        return Err(ChannelReadKeyError::Seal);
    }
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn array_at<const N: usize>(payload: &[u8], at: usize) -> CoreResult<[u8; N], ChannelReadKeyError> {
    payload
        .get(at..at + N)
        .and_then(|s| s.try_into().ok())
        .ok_or(ChannelReadKeyError::BadLength)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`ChannelReadKeyError::UnknownVersion`] (checked before the length) and
/// [`ChannelReadKeyError::BadLength`].
pub fn try_unwrap_channel_read_key(
    plaintext: &[u8],
) -> CoreResult<Option<ChannelReadKeyEnvelope>, ChannelReadKeyError> {
    let Some(payload) = plaintext.strip_prefix(CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, _) = payload
        .split_first()
        .ok_or(ChannelReadKeyError::BadLength)?;
    if version != CHANNEL_READ_KEY_VERSION_V1 {
        return Err(ChannelReadKeyError::UnknownVersion(version));
    }
    if payload.len() != CHANNEL_READ_KEY_PAYLOAD_LEN {
        return Err(ChannelReadKeyError::BadLength);
    }
    Ok(Some(ChannelReadKeyEnvelope {
        channel_id: array_at(payload, CHANNEL_AT)?,
        member_account_key: array_at(payload, MEMBER_AT)?,
        sealed: array_at(payload, SEALED_AT)?,
    }))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`ChannelReadKeyError::WrongDomain`] and every error of
/// [`try_unwrap_channel_read_key`].
pub fn decode_channel_read_key(
    bytes: &[u8],
) -> CoreResult<ChannelReadKeyEnvelope, ChannelReadKeyError> {
    try_unwrap_channel_read_key(bytes)?.ok_or(ChannelReadKeyError::WrongDomain)
}

impl ChannelReadKeyEnvelope {
    /// Whether this envelope is addressed to `account_key`.
    #[must_use]
    pub fn is_addressed_to(&self, account_key: &[u8; 32]) -> bool {
        self.member_account_key == *account_key
    }

    /// The exact wire bytes (every field is fixed width).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = header(&self.channel_id, &self.member_account_key);
        out.extend_from_slice(&self.sealed);
        out
    }

    /// Open the read key with the addressee account's X25519 secret. The
    /// header is the AAD, so another channel or addressee fails to open.
    ///
    /// # Errors
    ///
    /// [`ChannelReadKeyError::Open`].
    pub fn open(
        &self,
        account_x25519_secret: &X25519SecretKey,
    ) -> CoreResult<[u8; CHANNEL_READ_KEY_LEN], ChannelReadKeyError> {
        let aad = header(&self.channel_id, &self.member_account_key);
        let mut plain = envelope::open(account_x25519_secret, &aad, &self.sealed)
            .map_err(|_| ChannelReadKeyError::Open)?;
        let key: Option<[u8; CHANNEL_READ_KEY_LEN]> = plain.as_slice().try_into().ok();
        plain.fill(0);
        key.ok_or(ChannelReadKeyError::Open)
    }
}

/// The fuzz oracle behind `fuzz_api::channel_read_key_decode`: `None` when
/// the decoder refuses; `Some(true)` when what decoded re-encodes to the same
/// bytes and opening it under a fixed key never panicked.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let env = decode_channel_read_key(bytes).ok()?;
    let _ = env.open(&X25519SecretKey::from_bytes([0x33; 32]));
    Some(env.to_bytes() == bytes && bytes.len() == CHANNEL_READ_KEY_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; CHANNEL_READ_KEY_CHANNEL_ID_LEN] = [0x0C; CHANNEL_READ_KEY_CHANNEL_ID_LEN];
    const MEMBER: [u8; 32] = [0x4B; 32];
    const KEY: [u8; 32] = [0x2E; 32];

    fn member_keys() -> (X25519SecretKey, X25519PublicKey) {
        let secret = X25519SecretKey::from_bytes([0x33; 32]);
        let public = secret.public_key();
        (secret, public)
    }

    fn wire() -> Vec<u8> {
        let (_, public) = member_keys();
        compose_channel_read_key(&CHANNEL, &MEMBER, &public, &KEY).unwrap()
    }

    #[test]
    fn compose_decode_open_round_trips_at_a_fixed_length() {
        let (secret, _) = member_keys();
        let bytes = wire();
        assert_eq!(bytes.len(), CHANNEL_READ_KEY_ENVELOPE_LEN);
        assert_eq!(CHANNEL_READ_KEY_ENVELOPE_LEN, 26 + 129);
        let env = decode_channel_read_key(&bytes).unwrap();
        assert_eq!(env.channel_id, CHANNEL);
        assert!(env.is_addressed_to(&MEMBER));
        assert!(!env.is_addressed_to(&[0; 32]));
        assert_eq!(env.open(&secret).unwrap(), KEY);
        assert_eq!(env.to_bytes(), bytes);
        assert_eq!(fuzz_decode(&bytes), Some(true));
    }

    #[test]
    fn the_key_is_not_in_the_clear_and_only_the_addressee_opens_it() {
        let bytes = wire();
        assert!(!bytes.windows(32).any(|w| w == KEY));
        let env = decode_channel_read_key(&bytes).unwrap();
        assert_eq!(
            env.open(&X25519SecretKey::from_bytes([0x44; 32])),
            Err(ChannelReadKeyError::Open)
        );
    }

    #[test]
    fn the_clear_header_is_bound_to_the_seal() {
        let (secret, _) = member_keys();
        let good = wire();
        let domain = CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1.len();
        for at in [domain + CHANNEL_AT, domain + MEMBER_AT] {
            let mut tampered = good.clone();
            tampered[at] ^= 0x01;
            let env = decode_channel_read_key(&tampered).unwrap();
            assert_eq!(
                env.open(&secret),
                Err(ChannelReadKeyError::Open),
                "byte {at}"
            );
        }
    }

    #[test]
    fn version_and_length_are_checked() {
        let good = wire();
        assert_eq!(
            try_unwrap_channel_read_key(&good[..good.len() - 1]),
            Err(ChannelReadKeyError::BadLength)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(
            try_unwrap_channel_read_key(&long),
            Err(ChannelReadKeyError::BadLength)
        );
        let mut v2 = good;
        v2[CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1.len()] = 0x02;
        assert_eq!(
            try_unwrap_channel_read_key(&v2),
            Err(ChannelReadKeyError::UnknownVersion(0x02))
        );
        assert_eq!(
            try_unwrap_channel_read_key(CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1),
            Err(ChannelReadKeyError::BadLength)
        );
        assert_eq!(try_unwrap_channel_read_key(b"hello"), Ok(None));
        assert_eq!(
            decode_channel_read_key(b"hello"),
            Err(ChannelReadKeyError::WrongDomain)
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains_and_dispatched() {
        for d in [
            crate::admission_seed::ADMISSION_SEED_ENVELOPE_DOMAIN_V1,
            crate::knock_seed::KNOCK_SEED_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::conversation_handle::CONVERSATION_HANDLE_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
        ] {
            assert!(
                !d.starts_with(CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1)
                    && !CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1.starts_with(d),
                "{d:?}"
            );
        }
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&wire()),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelReadKey(
                _
            ))
        ));
    }
}
