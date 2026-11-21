// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sealing to the moderator list ([ADR-0099] sections 2 and 3).
//!
//! A report's content, and a suspended person's appeal, are sealed so that
//! only the listed moderators open them. One random content key seals the
//! content; the content key is wrapped to each moderator's X25519 key.
//!
//! ```text
//! sealed   = list_version(8) ‖ wrap_count(1) ‖ wrap × wrap_count
//!            ‖ nonce(12) ‖ ct_len(4) ‖ ct
//! wrap     = moderator_id(16) ‖ eph_pub(32) ‖ wrapped_key(48)
//! wrap key = HKDF-SHA-256(ikm = X25519(eph, moderator), salt = "",
//!            info = WRAP_INFO ‖ purpose ‖ eph_pub ‖ moderator_pub ‖ moderator_id)
//! bound    = list_version(8) ‖ outer aad
//! wrapped  = ChaCha20-Poly1305(wrap key, nonce = 0, aad = bound, content key)
//! ct       = ChaCha20-Poly1305(content key, nonce, aad = BODY_AAD ‖ purpose ‖ bound, content)
//! ```
//!
//! The wrap key is used once (its ephemeral key is fresh), so a fixed nonce
//! is safe. The outer AAD is the caller's clear part (a report's
//! [`crate::report::ReportClear`] bytes), so a sealed part cannot be moved
//! under another clear part. The list version in the clear header is bound
//! too: a sealed part whose version was rewritten does not open.
//!
//! A moderator's X25519 private key is derived in the browser from the
//! security key's WebAuthn PRF output ([`moderator_report_secret`]); it is
//! never stored.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

use lakko_crypto::aead::{open, seal, AeadKey, Algorithm, NONCE_LEN};
use lakko_crypto::agreement::{PublicKey, SecretKey};
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::random;

use crate::wire::{put32, Reader};
use crate::ModerationError;

/// The HKDF info prefix of a wrap key. Frozen.
pub const WRAP_INFO: &[u8] = b"lakko/moderation/seal-wrap/v1\0";
/// The AAD prefix of the content. Frozen.
pub const BODY_AAD: &[u8] = b"lakko/moderation/seal-body/v1\0";
/// The HKDF info of the moderator report key. Frozen.
pub const REPORT_KEY_INFO: &[u8] = b"lakko/moderation/report-key/v1";
/// The input a moderator's security key evaluates the WebAuthn PRF extension
/// over (`prf: { eval: { first: MODERATOR_PRF_INPUT } }`, these raw bytes).
/// Enrolment (where the list's X25519 key is computed from the output) and
/// the panel's sign-in must both use exactly these bytes, or the panel
/// derives a different key and opens no report. The browser hashes it with
/// the `"WebAuthn PRF\0"` prefix before the authenticator sees it. Frozen.
/// Equal to [`REPORT_KEY_INFO`] by choice; the two play different roles.
pub const MODERATOR_PRF_INPUT: &[u8] = b"lakko/moderation/report-key/v1";

/// A moderator id.
pub const MODERATOR_ID_LEN: usize = 16;
/// One wrap: id, ephemeral key, wrapped content key with its tag.
pub const WRAP_LEN: usize = MODERATOR_ID_LEN + 32 + 32 + 16;
/// The most wraps a sealed part carries (the list's own bound).
pub const MAX_WRAPS: usize = 64;
/// The longest sealed content (256 KiB, chosen, not measured).
pub const MAX_SEALED_CONTENT: usize = 256 * 1024;

/// What a seal is for; part of every key derivation, so a report's seal
/// never opens as an appeal's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Purpose {
    /// A report's content.
    Report = 1,
    /// A suspended person's appeal.
    Appeal = 2,
}

/// One recipient: a moderator id and its X25519 public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    /// The moderator id from the list.
    pub moderator_id: [u8; MODERATOR_ID_LEN],
    /// The moderator's X25519 public key from the list.
    pub x25519_public: [u8; 32],
}

/// Derive a moderator's X25519 report key from the 32-byte PRF output of
/// their security key.
///
/// # Errors
///
/// [`ModerationError::Invalid`] for a PRF output that is not 32 bytes.
pub fn moderator_report_secret(prf_output: &[u8]) -> Result<SecretKey, ModerationError> {
    if prf_output.len() != 32 {
        return Err(ModerationError::Invalid("prf output length"));
    }
    let okm = hkdf_sha256::<32>(prf_output, b"", REPORT_KEY_INFO)
        .map_err(|_| ModerationError::Invalid("kdf"))?;
    Ok(SecretKey::from_bytes(*okm.as_bytes()))
}

fn wrap_key(
    shared: &[u8; 32],
    purpose: Purpose,
    eph_pub: &[u8; 32],
    recipient: &Recipient,
) -> Result<AeadKey, ModerationError> {
    if shared.iter().all(|b| *b == 0) {
        return Err(ModerationError::Invalid("low-order key"));
    }
    let mut info = Vec::with_capacity(WRAP_INFO.len() + 1 + 32 + 32 + MODERATOR_ID_LEN);
    info.extend_from_slice(WRAP_INFO);
    info.push(purpose as u8);
    info.extend_from_slice(eph_pub);
    info.extend_from_slice(&recipient.x25519_public);
    info.extend_from_slice(&recipient.moderator_id);
    let okm = hkdf_sha256::<32>(shared, b"", &info).map_err(|_| ModerationError::Invalid("kdf"))?;
    Ok(AeadKey::new(*okm.as_bytes()))
}

/// The list version and the caller's clear part: the AAD of a wrap, and the
/// tail of the content's AAD.
fn bound_aad(list_version: u64, outer_aad: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(8 + outer_aad.len());
    aad.extend_from_slice(&list_version.to_be_bytes());
    aad.extend_from_slice(outer_aad);
    aad
}

fn body_aad(purpose: Purpose, bound: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(BODY_AAD.len() + 1 + bound.len());
    aad.extend_from_slice(BODY_AAD);
    aad.push(purpose as u8);
    aad.extend_from_slice(bound);
    aad
}

/// Seal `content` to every recipient.
///
/// # Errors
///
/// [`ModerationError::Invalid`] for no recipient, more than [`MAX_WRAPS`],
/// a duplicate moderator id, content over [`MAX_SEALED_CONTENT`], or a
/// low-order recipient key.
pub fn seal_for_moderators(
    purpose: Purpose,
    outer_aad: &[u8],
    content: &[u8],
    list_version: u64,
    recipients: &[Recipient],
) -> Result<Vec<u8>, ModerationError> {
    if recipients.is_empty() || recipients.len() > MAX_WRAPS {
        return Err(ModerationError::Invalid("recipient count"));
    }
    for (i, a) in recipients.iter().enumerate() {
        if recipients[..i]
            .iter()
            .any(|b| b.moderator_id == a.moderator_id)
        {
            return Err(ModerationError::Invalid("duplicate moderator"));
        }
    }
    if content.len() > MAX_SEALED_CONTENT {
        return Err(ModerationError::Invalid("content too long"));
    }
    let content_key = random::secret_bytes::<32>();
    let bound = bound_aad(list_version, outer_aad);
    let mut out = Vec::with_capacity(
        8 + 1 + recipients.len() * WRAP_LEN + NONCE_LEN + 4 + content.len() + 16,
    );
    out.extend_from_slice(&list_version.to_be_bytes());
    out.push(u8::try_from(recipients.len()).map_err(|_| ModerationError::Invalid("count"))?);
    for recipient in recipients {
        let eph = SecretKey::generate();
        let eph_pub = eph.public_key().to_bytes();
        let shared = eph
            .diffie_hellman(&PublicKey::from_array(recipient.x25519_public))
            .map_err(|_| ModerationError::Invalid("moderator key"))?;
        let key = wrap_key(shared.as_bytes(), purpose, &eph_pub, recipient)?;
        let wrapped = seal(
            Algorithm::ChaCha20Poly1305,
            &key,
            &[0u8; NONCE_LEN],
            &bound,
            content_key.as_bytes(),
        )
        .map_err(|_| ModerationError::Invalid("seal"))?;
        out.extend_from_slice(&recipient.moderator_id);
        out.extend_from_slice(&eph_pub);
        out.extend_from_slice(&wrapped);
    }
    let mut nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let ct = seal(
        Algorithm::ChaCha20Poly1305,
        &AeadKey::new(*content_key.as_bytes()),
        &nonce,
        &body_aad(purpose, &bound),
        content,
    )
    .map_err(|_| ModerationError::Invalid("seal"))?;
    out.extend_from_slice(&nonce);
    put32(&mut out, &ct, MAX_SEALED_CONTENT + 16)?;
    Ok(out)
}

/// A sealed part split into its fields, without opening anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedParts<'a> {
    /// The moderator-list version the seal was made under.
    pub list_version: u64,
    /// The moderator ids it is wrapped to, in wire order.
    pub moderator_ids: Vec<[u8; MODERATOR_ID_LEN]>,
    wraps: &'a [u8],
    nonce: [u8; NONCE_LEN],
    ct: &'a [u8],
}

/// Split a sealed part. Exact: trailing bytes are refused.
///
/// # Errors
///
/// [`ModerationError::Malformed`].
pub fn split(sealed: &[u8]) -> Result<SealedParts<'_>, ModerationError> {
    let mut r = Reader::new(sealed);
    let list_version = r.u64()?;
    let count = usize::from(r.u8()?);
    if count == 0 || count > MAX_WRAPS {
        return Err(ModerationError::Malformed("wrap count"));
    }
    let wraps = r.take(count * WRAP_LEN)?;
    let moderator_ids = wraps
        .chunks_exact(WRAP_LEN)
        .map(|w| {
            let mut id = [0u8; MODERATOR_ID_LEN];
            id.copy_from_slice(&w[..MODERATOR_ID_LEN]);
            id
        })
        .collect();
    let nonce = r.array::<NONCE_LEN>()?;
    let ct = r.bytes32(MAX_SEALED_CONTENT + 16)?;
    r.finish()?;
    Ok(SealedParts {
        list_version,
        moderator_ids,
        wraps,
        nonce,
        ct,
    })
}

/// Open a sealed part as the moderator `moderator_id` holding `secret`.
///
/// # Errors
///
/// [`ModerationError::Malformed`] for bad framing, [`ModerationError::CannotOpen`]
/// when the seal is not wrapped to this moderator or does not authenticate.
pub fn open_for_moderator(
    purpose: Purpose,
    outer_aad: &[u8],
    sealed: &[u8],
    moderator_id: &[u8; MODERATOR_ID_LEN],
    secret: &SecretKey,
) -> Result<Vec<u8>, ModerationError> {
    let parts = split(sealed)?;
    let wrap = parts
        .wraps
        .chunks_exact(WRAP_LEN)
        .find(|w| &w[..MODERATOR_ID_LEN] == moderator_id)
        .ok_or(ModerationError::CannotOpen)?;
    let mut eph_pub = [0u8; 32];
    eph_pub.copy_from_slice(&wrap[MODERATOR_ID_LEN..MODERATOR_ID_LEN + 32]);
    let recipient = Recipient {
        moderator_id: *moderator_id,
        x25519_public: secret.public_key().to_bytes(),
    };
    let shared = secret
        .diffie_hellman(&PublicKey::from_array(eph_pub))
        .map_err(|_| ModerationError::CannotOpen)?;
    let key = wrap_key(shared.as_bytes(), purpose, &eph_pub, &recipient)
        .map_err(|_| ModerationError::CannotOpen)?;
    let bound = bound_aad(parts.list_version, outer_aad);
    let content_key = open(
        Algorithm::ChaCha20Poly1305,
        &key,
        &[0u8; NONCE_LEN],
        &bound,
        &wrap[MODERATOR_ID_LEN + 32..],
    )
    .map_err(|_| ModerationError::CannotOpen)?;
    let content_key: [u8; 32] = content_key
        .as_slice()
        .try_into()
        .map_err(|_| ModerationError::CannotOpen)?;
    open(
        Algorithm::ChaCha20Poly1305,
        &AeadKey::new(content_key),
        &parts.nonce,
        &body_aad(purpose, &bound),
        parts.ct,
    )
    .map_err(|_| ModerationError::CannotOpen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prf_input_is_frozen() {
        assert_eq!(MODERATOR_PRF_INPUT, b"lakko/moderation/report-key/v1");
    }

    fn moderator(n: u8) -> (Recipient, SecretKey) {
        let secret = moderator_report_secret(&[n; 32]).unwrap();
        (
            Recipient {
                moderator_id: [n; 16],
                x25519_public: secret.public_key().to_bytes(),
            },
            secret,
        )
    }

    #[test]
    fn every_listed_moderator_opens_it_and_nobody_else() {
        let (a, sa) = moderator(1);
        let (b, sb) = moderator(2);
        let (_, sc) = moderator(3);
        let sealed =
            seal_for_moderators(Purpose::Report, b"clear", b"content", 7, &[a, b]).unwrap();
        assert_eq!(split(&sealed).unwrap().list_version, 7);
        for (id, s) in [([1u8; 16], &sa), ([2u8; 16], &sb)] {
            assert_eq!(
                open_for_moderator(Purpose::Report, b"clear", &sealed, &id, s).unwrap(),
                b"content"
            );
        }
        assert_eq!(
            open_for_moderator(Purpose::Report, b"clear", &sealed, &[3u8; 16], &sc),
            Err(ModerationError::CannotOpen)
        );
        assert_eq!(
            open_for_moderator(Purpose::Report, b"clear", &sealed, &[1u8; 16], &sc),
            Err(ModerationError::CannotOpen)
        );
    }

    #[test]
    fn another_clear_part_or_purpose_does_not_open() {
        let (a, sa) = moderator(1);
        let sealed = seal_for_moderators(Purpose::Report, b"clear", b"x", 1, &[a]).unwrap();
        assert!(open_for_moderator(Purpose::Report, b"other", &sealed, &[1u8; 16], &sa).is_err());
        assert!(open_for_moderator(Purpose::Appeal, b"clear", &sealed, &[1u8; 16], &sa).is_err());
    }

    #[test]
    fn a_rewritten_list_version_does_not_open() {
        let (a, sa) = moderator(1);
        let sealed = seal_for_moderators(Purpose::Report, b"clear", b"x", 5, &[a]).unwrap();
        assert!(open_for_moderator(Purpose::Report, b"clear", &sealed, &[1u8; 16], &sa).is_ok());
        for version in [4u64, 6, 0, u64::MAX] {
            let mut moved = sealed.clone();
            moved[..8].copy_from_slice(&version.to_be_bytes());
            assert_eq!(split(&moved).unwrap().list_version, version);
            assert_eq!(
                open_for_moderator(Purpose::Report, b"clear", &moved, &[1u8; 16], &sa),
                Err(ModerationError::CannotOpen)
            );
        }
    }

    #[test]
    fn bounds() {
        let (a, _) = moderator(1);
        assert!(seal_for_moderators(Purpose::Report, b"", b"x", 1, &[]).is_err());
        assert!(
            seal_for_moderators(Purpose::Report, b"", b"x", 1, &[a.clone(), a.clone()]).is_err()
        );
        assert!(seal_for_moderators(
            Purpose::Report,
            b"",
            &vec![0u8; MAX_SEALED_CONTENT + 1],
            1,
            std::slice::from_ref(&a)
        )
        .is_err());
        let low = Recipient {
            moderator_id: [9; 16],
            x25519_public: [0u8; 32],
        };
        assert!(seal_for_moderators(Purpose::Report, b"", b"x", 1, &[low]).is_err());
        let sealed = seal_for_moderators(Purpose::Report, b"", b"x", 1, &[a]).unwrap();
        let mut extra = sealed.clone();
        extra.push(0);
        assert!(split(&extra).is_err());
        for n in 0..sealed.len() {
            assert!(split(&sealed[..n]).is_err());
        }
    }

    #[test]
    fn the_prf_key_is_deterministic() {
        let a = moderator_report_secret(&[5; 32]).unwrap();
        let b = moderator_report_secret(&[5; 32]).unwrap();
        assert_eq!(a.public_key().to_bytes(), b.public_key().to_bytes());
        assert!(moderator_report_secret(&[5; 31]).is_err());
    }
}
