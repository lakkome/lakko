// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The sealed form every invite link carries (`/i/`, `/c/`, `/g/`) — the
//! 2026-09-28 security-pass amendment of ADR-0028 and ADR-0009.
//!
//! A link is `https://<host>/<k>/<id>#<secret>`, a pointer into the discovery
//! ledger (the inline form that carried the sealed blob in the path was
//! removed on 2026-10-19: a failed upload mints no link). The **signed capability is
//! encrypted** under a key derived from a random [`LINK_SECRET_LEN`]-byte
//! secret that rides in the URL **fragment**. A fragment never reaches a
//! server, so the discovery ledger stores ciphertext only, and a link without
//! its fragment opens nothing.
//!
//! What the seal closes (the three findings the 2026-09-28 pass left open):
//!
//! - **A 48-bit ledger id is no longer a bearer.** Fetching `/c/<id>` by
//!   guessing the id yields a blob that does not open without the 96-bit
//!   secret, so a guessed id is not a seat in an auto-admitted call.
//! - **The ledger cannot substitute a capability.** The AEAD tag binds the
//!   blob to the secret; a malicious discovery tier does not hold the secret,
//!   so any other blob it serves under a live id fails to open (for `/i/` and
//!   `/c/` as well as `/g/`).
//! - **A `/g/` link without its fragment no longer opens at all**, instead of
//!   accepting whatever capability the ledger returns.
//!
//! ## Wire
//!
//! ```text
//! u8          kind        — LinkKind byte: b'i' contact, b'c' call, b'g' group
//! u8          version     — LINK_SEAL_VERSION (= 1)
//! [12 bytes]  nonce       — CSPRNG, fresh per link
//! [..]        ciphertext  — ChaCha20-Poly1305(key, nonce, aad = kind ‖ version,
//!                                             plaintext = the signed capability wire)
//! [16 bytes]  tag
//! ```
//!
//! ```text
//! key = HKDF-SHA256(ikm  = link_secret (12 bytes),
//!                   salt = nonce,
//!                   info = LinkKind::key_domain())
//! ```
//!
//! The secret is uniform random, so HKDF only has to expand it. The per-link
//! nonce as the HKDF salt makes every link's derivation distinct: a guessed
//! secret has to be run through HKDF once per target link, so a search over
//! secrets cannot be amortised across links. The per-kind `info` keeps the
//! three kinds' keys apart even under one secret, and the kind byte in the AAD
//! refuses a blob presented under the wrong path (`/c/` bytes behind `/g/`).
//!
//! ChaCha20-Poly1305 is not key-committing. That does not matter here: the
//! party that could exploit non-commitment is one who knows candidate keys,
//! and the substitution adversary (the ledger) knows none — it cannot produce
//! a blob that opens under a secret it never saw.

use lakko_crypto::aead::{
    self, AeadKey, Algorithm, Nonce, KEY_LEN as AEAD_KEY_LEN, NONCE_LEN, TAG_LEN,
};
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::random;

use crate::error::{ContactError, Result};

/// Length of a link secret: 96 bits of CSPRNG output that ride in the URL
/// fragment (the short-link amendment of ADR-0028, 2026-09-27, extended to
/// every link kind on 2026-09-28).
pub const LINK_SECRET_LEN: usize = 12;

/// Length of a link secret in its URL-fragment form: base64url without
/// padding of [`LINK_SECRET_LEN`] bytes. Any other fragment length is a
/// damaged link.
pub const LINK_SECRET_B64URL_LEN: usize = 16;

/// Sealed-envelope version byte.
pub const LINK_SEAL_VERSION: u8 = 1;

/// Bytes the seal adds around the capability: kind, version, nonce, tag.
pub const LINK_SEAL_OVERHEAD: usize = 1 + 1 + NONCE_LEN + TAG_LEN;

/// HKDF `info` for a contact-invite (`/i/`) link key.
pub const CONTACT_LINK_KEY_DOMAIN_V1: &[u8] = b"lakko/invite-link/i/seal-key/v1\x00";
/// HKDF `info` for a call-invite (`/c/`) link key.
pub const CALL_LINK_KEY_DOMAIN_V1: &[u8] = b"lakko/invite-link/c/seal-key/v1\x00";
/// HKDF `info` for a group-invite (`/g/`) link key.
pub const GROUP_LINK_KEY_DOMAIN_V1: &[u8] = b"lakko/invite-link/g/seal-key/v1\x00";

/// Which capability a sealed link carries. The byte is the first byte of the
/// sealed wire and part of the AEAD associated data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// `/i/` — [`crate::invite::InviteCapability`].
    Contact,
    /// `/c/` — [`crate::call_invite::CallInviteCapability`].
    Call,
    /// `/g/` — [`crate::group_invite::GroupInviteCapability`].
    Group,
}

impl LinkKind {
    /// The wire byte (the path letter).
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::Contact => b'i',
            Self::Call => b'c',
            Self::Group => b'g',
        }
    }

    /// The HKDF `info` of this kind's link key.
    #[must_use]
    pub const fn key_domain(self) -> &'static [u8] {
        match self {
            Self::Contact => CONTACT_LINK_KEY_DOMAIN_V1,
            Self::Call => CALL_LINK_KEY_DOMAIN_V1,
            Self::Group => GROUP_LINK_KEY_DOMAIN_V1,
        }
    }
}

/// The sealed length of a capability of `capability_len` bytes.
#[must_use]
pub const fn sealed_len(capability_len: usize) -> usize {
    capability_len + LINK_SEAL_OVERHEAD
}

/// Mint a fresh link secret. One per link, never reused.
#[must_use]
pub fn generate_link_secret() -> [u8; LINK_SECRET_LEN] {
    let mut secret = [0u8; LINK_SECRET_LEN];
    random::fill(&mut secret);
    secret
}

/// Copy a caller-supplied secret into the fixed width, or refuse it as a
/// damaged link (a fragment of the wrong length is one).
///
/// # Errors
///
/// [`ContactError::LinkDamaged`] when `secret` is not [`LINK_SECRET_LEN`]
/// bytes.
pub fn link_secret_from_slice(secret: &[u8]) -> Result<[u8; LINK_SECRET_LEN]> {
    <[u8; LINK_SECRET_LEN]>::try_from(secret).map_err(|_| ContactError::LinkDamaged)
}

/// Compare two link secrets in constant time (ADR-0079 section 3: the
/// inviter's device checks a request's link answer against its own record
/// without a timing side channel on the secret).
#[must_use]
pub fn link_secrets_match(a: &[u8; LINK_SECRET_LEN], b: &[u8; LINK_SECRET_LEN]) -> bool {
    lakko_crypto::secret::SecretBytes::new(*a) == lakko_crypto::secret::SecretBytes::new(*b)
}

fn link_key(kind: LinkKind, secret: &[u8; LINK_SECRET_LEN], nonce: &Nonce) -> Result<AeadKey> {
    let key = hkdf_sha256::<AEAD_KEY_LEN>(secret, nonce, kind.key_domain())?;
    Ok(key)
}

/// Seal a signed capability wire under `secret`.
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when `capability` is empty.
/// - [`ContactError::Crypto`] on a primitive failure.
pub fn seal(kind: LinkKind, secret: &[u8; LINK_SECRET_LEN], capability: &[u8]) -> Result<Vec<u8>> {
    if capability.is_empty() {
        return Err(ContactError::InvalidArgument(
            "a sealed link needs a non-empty capability".into(),
        ));
    }
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let key = link_key(kind, secret, &nonce)?;
    let aad = [kind.byte(), LINK_SEAL_VERSION];
    let ciphertext = aead::seal(Algorithm::ChaCha20Poly1305, &key, &nonce, &aad, capability)?;
    let mut out = Vec::with_capacity(2 + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&aad);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Open a sealed link and return the signed capability wire inside it. The
/// caller then runs the kind's `decode_and_verify` on the result.
///
/// Every refusal is the one [`ContactError::LinkDamaged`]: a wrong or missing
/// secret, a blob of another kind, a truncated blob, a swapped blob and a
/// tampered one are indistinguishable to the person holding the link, and the
/// page says the same thing for all of them.
///
/// # Errors
///
/// [`ContactError::LinkDamaged`] on any failure.
pub fn open(kind: LinkKind, secret: &[u8; LINK_SECRET_LEN], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() <= LINK_SEAL_OVERHEAD {
        return Err(ContactError::LinkDamaged);
    }
    let (aad, rest) = sealed.split_at(2);
    if aad[0] != kind.byte() || aad[1] != LINK_SEAL_VERSION {
        return Err(ContactError::LinkDamaged);
    }
    let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: Nonce =
        <[u8; NONCE_LEN]>::try_from(nonce_bytes).map_err(|_| ContactError::LinkDamaged)?;
    let key = link_key(kind, secret, &nonce).map_err(|_| ContactError::LinkDamaged)?;
    aead::open(Algorithm::ChaCha20Poly1305, &key, &nonce, aad, ciphertext)
        .map_err(|_| ContactError::LinkDamaged)
}

/// Open a sealed link and verify the capability inside with the kind's
/// decoder, folding every failure into [`ContactError::LinkDamaged`]. Returns
/// the verified capability and its plaintext wire (the joiner tag and the
/// `/g/` join attribution are computed over the plaintext).
///
/// # Errors
///
/// [`ContactError::LinkDamaged`] on any failure.
pub(crate) fn open_and_verify<T>(
    kind: LinkKind,
    secret: &[u8; LINK_SECRET_LEN],
    sealed: &[u8],
    decode: impl FnOnce(&[u8]) -> Result<T>,
) -> Result<(T, Vec<u8>)> {
    let wire = open(kind, secret, sealed)?;
    let cap = decode(&wire).map_err(|_| ContactError::LinkDamaged)?;
    Ok((cap, wire))
}

/// Fuzz hook behind [`crate::fuzz_api::open_sealed_link`]: open a sealed
/// link of the kind `kind_byte` names (any other byte picks by `kind_byte % 3`)
/// under `secret`, verify the capability inside, and check that whatever was
/// accepted re-seals and re-opens to the same plaintext and decodes the same.
pub(crate) fn fuzz_open(kind_byte: u8, secret: &[u8], sealed: &[u8]) -> Option<bool> {
    let kind = match kind_byte % 3 {
        0 => LinkKind::Contact,
        1 => LinkKind::Call,
        _ => LinkKind::Group,
    };
    let secret = link_secret_from_slice(secret).ok()?;
    let wire = open(kind, &secret, sealed).ok()?;
    let verified = match kind {
        LinkKind::Contact => crate::invite::decode_and_verify(&wire).map(|_| ()),
        LinkKind::Call => crate::call_invite::decode_and_verify(&wire).map(|_| ()),
        LinkKind::Group => crate::group_invite::decode_and_verify(&wire).map(|_| ()),
    };
    verified.ok()?;
    let resealed = seal(kind, &secret, &wire).ok()?;
    Some(open(kind, &secret, &resealed).ok().as_deref() == Some(&wire[..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; LINK_SECRET_LEN] = [0x42; LINK_SECRET_LEN];

    #[test]
    fn seal_then_open_round_trips_for_every_kind() {
        for kind in [LinkKind::Contact, LinkKind::Call, LinkKind::Group] {
            let sealed = seal(kind, &SECRET, b"capability").unwrap();
            assert_eq!(sealed.len(), sealed_len(b"capability".len()));
            assert_eq!(sealed[0], kind.byte());
            assert_eq!(open(kind, &SECRET, &sealed).unwrap(), b"capability");
        }
    }

    #[test]
    fn a_wrong_secret_is_damaged() {
        let sealed = seal(LinkKind::Call, &SECRET, b"capability").unwrap();
        let mut other = SECRET;
        other[0] ^= 1;
        assert!(matches!(
            open(LinkKind::Call, &other, &sealed),
            Err(ContactError::LinkDamaged)
        ));
    }

    #[test]
    fn a_blob_under_another_path_is_damaged() {
        let sealed = seal(LinkKind::Call, &SECRET, b"capability").unwrap();
        assert!(matches!(
            open(LinkKind::Group, &SECRET, &sealed),
            Err(ContactError::LinkDamaged)
        ));
        // Relabelling the kind byte does not help: the key and the AAD move with it.
        let mut relabelled = sealed;
        relabelled[0] = b'g';
        assert!(matches!(
            open(LinkKind::Group, &SECRET, &relabelled),
            Err(ContactError::LinkDamaged)
        ));
    }

    #[test]
    fn a_swapped_blob_is_damaged() {
        // The ledger holds some other valid sealed link; served under this
        // link's id, it does not open under this link's secret.
        let theirs = seal(
            LinkKind::Contact,
            &generate_link_secret(),
            b"other capability",
        )
        .unwrap();
        assert!(matches!(
            open(LinkKind::Contact, &SECRET, &theirs),
            Err(ContactError::LinkDamaged)
        ));
    }

    #[test]
    fn tampering_and_truncation_are_damaged() {
        let sealed = seal(LinkKind::Contact, &SECRET, b"capability").unwrap();
        for i in 0..sealed.len() {
            let mut t = sealed.clone();
            t[i] ^= 0x80;
            assert!(open(LinkKind::Contact, &SECRET, &t).is_err(), "byte {i}");
        }
        for n in 0..sealed.len() {
            assert!(
                open(LinkKind::Contact, &SECRET, &sealed[..n]).is_err(),
                "len {n}"
            );
        }
        let mut version = sealed;
        version[1] = LINK_SEAL_VERSION + 1;
        assert!(open(LinkKind::Contact, &SECRET, &version).is_err());
    }

    #[test]
    fn two_seals_of_one_capability_differ() {
        let a = seal(LinkKind::Group, &SECRET, b"capability").unwrap();
        let b = seal(LinkKind::Group, &SECRET, b"capability").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_secret_of_the_wrong_length_is_damaged() {
        assert!(link_secret_from_slice(&[0u8; LINK_SECRET_LEN]).is_ok());
        for n in [0, 11, 13, 32] {
            assert!(matches!(
                link_secret_from_slice(&vec![0u8; n]),
                Err(ContactError::LinkDamaged)
            ));
        }
    }

    #[test]
    fn the_empty_capability_is_refused_at_seal() {
        assert!(seal(LinkKind::Contact, &SECRET, b"").is_err());
    }

    #[test]
    fn the_secret_url_length_matches_its_width() {
        assert_eq!(LINK_SECRET_B64URL_LEN, (LINK_SECRET_LEN * 4).div_ceil(3));
    }
}
