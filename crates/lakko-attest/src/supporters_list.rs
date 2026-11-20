// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Supporters list — the signed list of backer names ([ADR-0097] section 2).
//!
//! A static file served at `/.well-known/lakko-supporters`, signed offline
//! by the release publisher key of [ADR-0038] under its own domain, so the
//! clients' existing release-key pin verifies it. Like the release manifest
//! it is global (no account) and only ever client-verified.
//!
//! ```text
//! wire          = format(1) = 0x01 ‖ list_version(8, BE u64)
//!                 ‖ signed_at_unix_millis(8, BE i64) ‖ count(4, BE u32)
//!                 ‖ count × (len(1) ‖ UTF-8 name) ‖ Ed25519_sig(64)
//! signing input = DOMAIN ‖ every wire byte before the signature
//! DOMAIN        = b"lakko/supporters-list/v1"
//! ```
//!
//! Each name follows the ADR-0048 display-name rule
//! ([`lakko_contacts::check_display_name`]: trimmed, 1-64 characters, at most
//! 255 bytes, no line break, control or format character). At most
//! [`MAX_SUPPORTERS`] names and [`MAX_SUPPORTERS_LIST_LEN`] bytes in all
//! (chosen, not measured); both are checked before anything is allocated
//! for the names. The parser refuses trailing bytes, so one list has one
//! encoding.
//!
//! [ADR-0038]: ../../../docs/adr/0038-update-available-notice.md
//! [ADR-0097]: ../../../docs/adr/0097-supporter-codes-and-list.md

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

/// Domain separator mixed into the signing input. Distinct from every other
/// kind in this crate, so a supporters list never verifies as one of them
/// (and the reverse) even under the shared publisher key.
const DOMAIN: &[u8] = b"lakko/supporters-list/v1";

/// Wire-format version. The parser refuses any other value.
const FORMAT_VERSION: u8 = 1;

/// Bytes before the first name: format, list version, signed-at, count.
const HEADER_LEN: usize = 1 + 8 + 8 + 4;

/// Most names in a list.
pub const MAX_SUPPORTERS: u32 = 100_000;

/// Most bytes in a whole list, signature included.
pub const MAX_SUPPORTERS_LIST_LEN: usize = 2 * 1024 * 1024;

/// Smallest possible list: no names.
pub const MIN_SUPPORTERS_LIST_LEN: usize = HEADER_LEN + SIGNATURE_LEN;

/// Why a supporters list was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SupportersListError {
    /// Shorter than the smallest list or longer than
    /// [`MAX_SUPPORTERS_LIST_LEN`].
    #[error("supporters list has a length out of bounds: {got} bytes")]
    BadLength {
        /// The length received.
        got: usize,
    },
    /// The format byte is not 1.
    #[error("unsupported supporters list format: {0}")]
    BadVersion(u8),
    /// More than [`MAX_SUPPORTERS`] names, or a count the bytes cannot hold.
    #[error("supporters list count is out of bounds")]
    BadCount,
    /// A name runs past the end, the bytes do not end at the signature, or
    /// a name is not UTF-8.
    #[error("supporters list is malformed")]
    Malformed,
    /// A name breaks the display-name rule (index in the list).
    #[error("supporters list name {0} is not a valid display name")]
    BadName(usize),
    /// The signature does not verify under the given key.
    #[error("supporters list signature is invalid")]
    BadSignature,
}

/// The authenticated content of a supporters list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupportersList {
    /// Monotone version; a client keeps the highest it verified.
    pub list_version: u64,
    /// When it was signed, Unix milliseconds (informational).
    pub signed_at_unix_millis: i64,
    /// The names, in the order of the file.
    pub names: Vec<String>,
}

impl SupportersList {
    /// The wire bytes before the signature, refusing what the parser
    /// refuses.
    fn body(&self) -> Result<Vec<u8>, SupportersListError> {
        let count = u32::try_from(self.names.len()).map_err(|_| SupportersListError::BadCount)?;
        if count > MAX_SUPPORTERS {
            return Err(SupportersListError::BadCount);
        }
        let mut out = Vec::with_capacity(HEADER_LEN);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&self.list_version.to_be_bytes());
        out.extend_from_slice(&self.signed_at_unix_millis.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        for (i, name) in self.names.iter().enumerate() {
            lakko_contacts::check_display_name(name)
                .map_err(|_| SupportersListError::BadName(i))?;
            let len = u8::try_from(name.len()).map_err(|_| SupportersListError::BadName(i))?;
            out.push(len);
            out.extend_from_slice(name.as_bytes());
        }
        if out.len() + SIGNATURE_LEN > MAX_SUPPORTERS_LIST_LEN {
            return Err(SupportersListError::BadLength {
                got: out.len() + SIGNATURE_LEN,
            });
        }
        Ok(out)
    }

    /// Serialize and sign (the offline publisher tool).
    ///
    /// # Errors
    ///
    /// The refusals [`Self::open`] would give the result: a bad name, too
    /// many names, or too many bytes.
    pub fn sign(&self, signing: &SigningKey) -> Result<Vec<u8>, SupportersListError> {
        let mut body = self.body()?;
        let mut input = Vec::with_capacity(DOMAIN.len() + body.len());
        input.extend_from_slice(DOMAIN);
        input.extend_from_slice(&body);
        let sig = signing.sign(&input);
        body.extend_from_slice(&sig.to_bytes());
        Ok(body)
    }

    /// Parse the wire form and verify its signature under `verifying`.
    ///
    /// Bounds are checked before the names are allocated; the signature is
    /// checked before any name is validated, so a forged list costs one
    /// signature check.
    ///
    /// # Errors
    ///
    /// Every [`SupportersListError`].
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, SupportersListError> {
        if bytes.len() < MIN_SUPPORTERS_LIST_LEN || bytes.len() > MAX_SUPPORTERS_LIST_LEN {
            return Err(SupportersListError::BadLength { got: bytes.len() });
        }
        if bytes[0] != FORMAT_VERSION {
            return Err(SupportersListError::BadVersion(bytes[0]));
        }
        let mut u64b = [0u8; 8];
        u64b.copy_from_slice(&bytes[1..9]);
        let list_version = u64::from_be_bytes(u64b);
        u64b.copy_from_slice(&bytes[9..17]);
        let signed_at_unix_millis = i64::from_be_bytes(u64b);
        let mut u32b = [0u8; 4];
        u32b.copy_from_slice(&bytes[17..HEADER_LEN]);
        let count = u32::from_be_bytes(u32b);
        let body_end = bytes.len() - SIGNATURE_LEN;
        // Every name takes at least two bytes (length + one byte).
        if count > MAX_SUPPORTERS
            || usize::try_from(count)
                .ok()
                .and_then(|c| c.checked_mul(2))
                .map_or(true, |need| need > body_end - HEADER_LEN)
        {
            return Err(SupportersListError::BadCount);
        }

        let sig = Signature::from_bytes(&bytes[body_end..])
            .map_err(|_| SupportersListError::BadSignature)?;
        let mut input = Vec::with_capacity(DOMAIN.len() + body_end);
        input.extend_from_slice(DOMAIN);
        input.extend_from_slice(&bytes[..body_end]);
        verifying
            .verify(&input, &sig)
            .map_err(|_| SupportersListError::BadSignature)?;

        let count = usize::try_from(count).map_err(|_| SupportersListError::BadCount)?;
        let mut names = Vec::with_capacity(count);
        let mut off = HEADER_LEN;
        for i in 0..count {
            let len = usize::from(*bytes.get(off).ok_or(SupportersListError::Malformed)?);
            off += 1;
            let end = off.checked_add(len).ok_or(SupportersListError::Malformed)?;
            if end > body_end {
                return Err(SupportersListError::Malformed);
            }
            let name = core::str::from_utf8(&bytes[off..end])
                .map_err(|_| SupportersListError::Malformed)?;
            lakko_contacts::check_display_name(name)
                .map_err(|_| SupportersListError::BadName(i))?;
            names.push(name.to_string());
            off = end;
        }
        if off != body_end {
            return Err(SupportersListError::Malformed);
        }
        Ok(Self {
            list_version,
            signed_at_unix_millis,
            names,
        })
    }
}

/// Verify a supporters list under the pinned release key (the client entry
/// point). No freshness: a client keeps the highest `list_version` it has
/// verified and ignores a lower one.
///
/// # Errors
///
/// As [`SupportersList::open`].
pub fn verify_supporters_list(
    bytes: &[u8],
    verifying: &VerifyingKey,
) -> Result<SupportersList, SupportersListError> {
    SupportersList::open(bytes, verifying)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionAttestation, BackupHead, GenesisAttestation, ReleaseManifest};
    use proptest::prelude::*;

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x5C; 32])
    }

    fn sample() -> SupportersList {
        SupportersList {
            list_version: 3,
            signed_at_unix_millis: 1_790_000_000_000,
            names: vec![
                "Ada".into(),
                "Мария".into(),
                "李雷".into(),
                "\u{1F600} fan".into(),
            ],
        }
    }

    #[test]
    fn sign_then_open_round_trips_with_the_documented_layout() {
        let sk = key();
        let wire = sample().sign(&sk).unwrap();
        assert_eq!(wire[0], 1);
        assert_eq!(&wire[1..9], &3u64.to_be_bytes());
        assert_eq!(&wire[17..21], &4u32.to_be_bytes());
        assert_eq!(wire[21], 3);
        assert_eq!(&wire[22..25], b"Ada");
        assert_eq!(
            verify_supporters_list(&wire, &sk.verifying_key()).unwrap(),
            sample()
        );
        let empty = SupportersList {
            names: vec![],
            ..sample()
        };
        let wire = empty.sign(&sk).unwrap();
        assert_eq!(wire.len(), MIN_SUPPORTERS_LIST_LEN);
        assert_eq!(
            SupportersList::open(&wire, &sk.verifying_key()).unwrap(),
            empty
        );
    }

    #[test]
    fn a_wrong_key_or_any_flipped_byte_is_refused() {
        let sk = key();
        let wire = sample().sign(&sk).unwrap();
        let other = SigningKey::from_seed(&[0x5D; 32]).verifying_key();
        assert_eq!(
            SupportersList::open(&wire, &other),
            Err(SupportersListError::BadSignature)
        );
        for i in 0..wire.len() {
            let mut bad = wire.clone();
            bad[i] ^= 0x01;
            assert!(
                SupportersList::open(&bad, &sk.verifying_key()).is_err(),
                "byte {i}"
            );
        }
        let mut long = wire.clone();
        long.push(0);
        assert!(SupportersList::open(&long, &sk.verifying_key()).is_err());
    }

    #[test]
    fn a_name_breaking_the_display_rule_is_refused_on_both_sides() {
        let sk = key();
        for bad in [
            "",
            " padded",
            "line\nbreak",
            "zero\u{200B}width",
            &"x".repeat(65),
        ] {
            let list = SupportersList {
                names: vec!["Ok".into(), bad.to_string()],
                ..sample()
            };
            assert_eq!(
                list.sign(&sk),
                Err(SupportersListError::BadName(1)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn counts_are_bounded_before_allocation() {
        let sk = key();
        let mut wire = sample().sign(&sk).unwrap();
        wire[17..21].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            SupportersList::open(&wire, &sk.verifying_key()),
            Err(SupportersListError::BadCount)
        );
        assert_eq!(
            SupportersList::open(&[1u8; 10], &sk.verifying_key()),
            Err(SupportersListError::BadLength { got: 10 })
        );
        let huge = vec![1u8; MAX_SUPPORTERS_LIST_LEN + 1];
        assert!(matches!(
            SupportersList::open(&huge, &sk.verifying_key()),
            Err(SupportersListError::BadLength { .. })
        ));
    }

    #[test]
    fn domains_do_not_cross_with_the_other_kinds() {
        // Under one shared key, a supporters list verifies as no other kind
        // and none of them as a list: the domain is in every signing input.
        let sk = key();
        let vk = sk.verifying_key();
        let foreign: [Vec<u8>; 4] = [
            ReleaseManifest::new(5, 1, 1_000).sign(&sk).to_vec(),
            BackupHead::new([1; 16], 2, [3; 32], 4).sign(&sk).to_vec(),
            GenesisAttestation::new([1; 16], [2; 32], [3; 32], 4)
                .sign(&sk)
                .to_vec(),
            ActionAttestation::new([1; 16], [2; 32], 3)
                .sign(&sk)
                .to_vec(),
        ];
        for blob in &foreign {
            assert!(SupportersList::open(blob, &vk).is_err());
        }
        // A list whose bytes, signature aside, mimic a release manifest's
        // length still never verifies as one.
        let list = SupportersList {
            names: vec![],
            ..sample()
        }
        .sign(&sk)
        .unwrap();
        assert!(ReleaseManifest::open(&list, &vk).is_err());
        // The same body re-signed WITHOUT the domain does not verify.
        let body = &list[..list.len() - SIGNATURE_LEN];
        let mut forged = body.to_vec();
        forged.extend_from_slice(&sk.sign(body).to_bytes());
        assert_eq!(
            SupportersList::open(&forged, &vk),
            Err(SupportersListError::BadSignature)
        );
    }

    proptest! {
        #[test]
        fn open_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = SupportersList::open(&bytes, &key().verifying_key());
        }

        #[test]
        fn valid_lists_round_trip(names in proptest::collection::vec("[A-Za-z][A-Za-z ]{0,20}[a-z]", 0..20), version in any::<u64>()) {
            let list = SupportersList { list_version: version, signed_at_unix_millis: 7, names };
            let sk = key();
            let wire = list.sign(&sk).unwrap();
            prop_assert_eq!(SupportersList::open(&wire, &sk.verifying_key()).unwrap(), list);
        }
    }
}
