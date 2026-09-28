// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The signed translation bundle (ADR-0058 section 5).
//!
//! A native client may receive translation fixes between releases. The
//! bundle carries **every** language in one payload, so fetching it tells
//! the server nothing about this device's language, and it is signed by a
//! dedicated offline publisher key pinned in the app.
//!
//! Wire layout:
//!
//! ```text
//! format_version(1) = 1
//! bundle_version(8, BE u64)
//! min_release(8, BE u64)
//! signed_at_unix_millis(8, BE i64)
//! payload_len(4, BE u32)
//! payload(payload_len)          JSON {"format":1,"languages":{code:{key:text}}}
//! sig(64)                       Ed25519 over DOMAIN || every byte above
//! ```
//!
//! Acceptance ([`accept_bundle`]) is all or nothing at the bundle level;
//! which strings of an accepted bundle may be used is decided per string by
//! [`Catalog::new`](crate::Catalog::new), which never lets one replace a
//! security-critical source string.

use std::collections::BTreeMap;

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use serde::{Deserialize, Serialize};

use crate::error::L10nError;
use crate::language::language;

/// Domain separator mixed into the signing input. Distinct from every
/// `lakko-attest` domain, so no other Lakko signature verifies as a bundle.
const DOMAIN: &[u8] = b"lakko/translation-bundle/v1";

/// The only wire format this build reads.
pub const FORMAT_VERSION: u8 = 1;

/// Header bytes before the payload.
pub const HEADER_LEN: usize = 1 + 8 + 8 + 8 + 4;

/// Largest bundle accepted, checked before anything is parsed. Chosen, not
/// measured: to be revisited once the first full set of translations exists
/// and its size is known.
pub const MAX_BUNDLE_BYTES: usize = 4 * 1024 * 1024;

const PAYLOAD_FORMAT: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    format: u32,
    languages: BTreeMap<String, BTreeMap<String, String>>,
}

/// What a device needs to decide whether to accept a bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptancePolicy {
    /// This build's release number (the ADR-0038 `CURRENT_RELEASE`).
    pub current_release: u64,
    /// The version of the catalog compiled into this build.
    pub built_in_version: u64,
    /// The version of the last bundle this device accepted, if any. Stored
    /// by the client so a replayed older bundle is refused.
    pub last_accepted: Option<u64>,
}

/// A bundle that verified and passed the version rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedBundle {
    version: u64,
    signed_at_millis: i64,
    languages: BTreeMap<String, BTreeMap<String, String>>,
}

impl AcceptedBundle {
    /// The bundle version; the client stores it as `last_accepted`.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// When it was signed, for Settings > About ("Translations updated").
    #[must_use]
    pub const fn signed_at_millis(&self) -> i64 {
        self.signed_at_millis
    }

    /// The strings for one language, if the bundle carries it.
    #[must_use]
    pub fn language(&self, code: &str) -> Option<&BTreeMap<String, String>> {
        self.languages.get(code)
    }
}

/// Serialise the payload of a bundle.
pub fn bundle_payload(
    languages: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<Vec<u8>, L10nError> {
    serde_json::to_vec(&Payload {
        format: PAYLOAD_FORMAT,
        languages: languages.clone(),
    })
    .map_err(|e| L10nError::Catalog(e.to_string()))
}

/// Sign a bundle. Offline publisher tool only; never on a client.
pub fn sign_bundle(
    key: &SigningKey,
    bundle_version: u64,
    min_release: u64,
    signed_at_millis: i64,
    payload: &[u8],
) -> Result<Vec<u8>, L10nError> {
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| L10nError::BundleMalformed("payload too large"))?;
    if HEADER_LEN + payload.len() + SIGNATURE_LEN > MAX_BUNDLE_BYTES {
        return Err(L10nError::BundleMalformed("bundle too large"));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + SIGNATURE_LEN);
    out.push(FORMAT_VERSION);
    out.extend_from_slice(&bundle_version.to_be_bytes());
    out.extend_from_slice(&min_release.to_be_bytes());
    out.extend_from_slice(&signed_at_millis.to_be_bytes());
    out.extend_from_slice(&payload_len.to_be_bytes());
    out.extend_from_slice(payload);
    let sig = key.sign(&signing_input(&out));
    out.extend_from_slice(&sig.to_bytes());
    Ok(out)
}

fn signing_input(signed: &[u8]) -> Vec<u8> {
    let mut input = Vec::with_capacity(DOMAIN.len() + signed.len());
    input.extend_from_slice(DOMAIN);
    input.extend_from_slice(signed);
    input
}

fn be_u64(b: &[u8]) -> Result<u64, L10nError> {
    let arr: [u8; 8] = b
        .try_into()
        .map_err(|_| L10nError::BundleMalformed("short field"))?;
    Ok(u64::from_be_bytes(arr))
}

/// Verify a bundle and apply the version rules. Checks, in order: size,
/// layout, signature, version newer than both the built-in catalog and the
/// last accepted bundle, release floor, payload shape. Languages this build
/// does not ship are dropped.
pub fn accept_bundle(
    bytes: &[u8],
    key: &VerifyingKey,
    policy: AcceptancePolicy,
) -> Result<AcceptedBundle, L10nError> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(L10nError::BundleMalformed("bundle too large"));
    }
    if bytes.len() < HEADER_LEN + SIGNATURE_LEN {
        return Err(L10nError::BundleMalformed("bundle too short"));
    }
    if bytes[0] != FORMAT_VERSION {
        return Err(L10nError::BundleMalformed("unknown format version"));
    }
    let bundle_version = be_u64(&bytes[1..9])?;
    let min_release = be_u64(&bytes[9..17])?;
    let signed_at_millis = i64::from_be_bytes(
        bytes[17..25]
            .try_into()
            .map_err(|_| L10nError::BundleMalformed("short field"))?,
    );
    let payload_len = u32::from_be_bytes(
        bytes[25..29]
            .try_into()
            .map_err(|_| L10nError::BundleMalformed("short field"))?,
    ) as usize;
    let expected = HEADER_LEN
        .checked_add(payload_len)
        .and_then(|n| n.checked_add(SIGNATURE_LEN))
        .ok_or(L10nError::BundleMalformed("length overflow"))?;
    if expected != bytes.len() {
        return Err(L10nError::BundleMalformed("length mismatch"));
    }
    let signed_end = HEADER_LEN + payload_len;
    let sig =
        Signature::from_bytes(&bytes[signed_end..]).map_err(|_| L10nError::BundleSignature)?;
    key.verify(&signing_input(&bytes[..signed_end]), &sig)
        .map_err(|_| L10nError::BundleSignature)?;

    let floor = policy
        .last_accepted
        .map_or(policy.built_in_version, |l| l.max(policy.built_in_version));
    if bundle_version <= floor {
        return Err(L10nError::BundleNotNewer {
            got: bundle_version,
            floor,
        });
    }
    if min_release > policy.current_release {
        return Err(L10nError::BundleTooNew {
            needed: min_release,
            current: policy.current_release,
        });
    }
    let payload: Payload = serde_json::from_slice(&bytes[HEADER_LEN..signed_end])
        .map_err(|_| L10nError::BundleMalformed("payload is not a bundle payload"))?;
    if payload.format != PAYLOAD_FORMAT {
        return Err(L10nError::BundleMalformed("unknown payload format"));
    }
    let languages = payload
        .languages
        .into_iter()
        .filter(|(code, _)| language(code).is_some())
        .collect();
    Ok(AcceptedBundle {
        version: bundle_version,
        signed_at_millis,
        languages,
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn bundle_with(version: u64, langs: &[(&str, &[(&str, &str)])]) -> AcceptedBundle {
        AcceptedBundle {
            version,
            signed_at_millis: 0,
            languages: langs
                .iter()
                .map(|(code, strings)| {
                    (
                        (*code).to_owned(),
                        strings
                            .iter()
                            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                            .collect(),
                    )
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_seed(&[7u8; 32])
    }

    fn payload() -> Vec<u8> {
        let mut langs = BTreeMap::new();
        langs.insert(
            "ru".to_owned(),
            BTreeMap::from([("k".to_owned(), "v".to_owned())]),
        );
        langs.insert(
            "de".to_owned(),
            BTreeMap::from([("k".to_owned(), "v".to_owned())]),
        );
        bundle_payload(&langs).unwrap()
    }

    const POLICY: AcceptancePolicy = AcceptancePolicy {
        current_release: 5,
        built_in_version: 10,
        last_accepted: None,
    };

    #[test]
    fn round_trip_drops_unsupported_languages() {
        let b = sign_bundle(&key(), 11, 5, 1_700_000_000_000, &payload()).unwrap();
        let accepted = accept_bundle(&b, &key().verifying_key(), POLICY).unwrap();
        assert_eq!(accepted.version(), 11);
        assert_eq!(accepted.signed_at_millis(), 1_700_000_000_000);
        assert!(accepted.language("ru").is_some());
        assert!(accepted.language("de").is_none());
    }

    #[test]
    fn wrong_key_or_tampering_is_refused() {
        let b = sign_bundle(&key(), 11, 5, 0, &payload()).unwrap();
        let other = SigningKey::from_seed(&[8u8; 32]).verifying_key();
        assert_eq!(
            accept_bundle(&b, &other, POLICY),
            Err(L10nError::BundleSignature)
        );
        for i in [0usize, 3, 12, 20, 27, HEADER_LEN + 2, b.len() - 1] {
            let mut t = b.clone();
            t[i] ^= 1;
            assert!(
                accept_bundle(&t, &key().verifying_key(), POLICY).is_err(),
                "byte {i}"
            );
        }
    }

    #[test]
    fn versions_must_move_forward() {
        let vk = key().verifying_key();
        let same_as_built_in = sign_bundle(&key(), 10, 5, 0, &payload()).unwrap();
        assert!(matches!(
            accept_bundle(&same_as_built_in, &vk, POLICY),
            Err(L10nError::BundleNotNewer { got: 10, floor: 10 })
        ));
        let b = sign_bundle(&key(), 12, 5, 0, &payload()).unwrap();
        let replay = AcceptancePolicy {
            last_accepted: Some(12),
            ..POLICY
        };
        assert!(matches!(
            accept_bundle(&b, &vk, replay),
            Err(L10nError::BundleNotNewer { got: 12, floor: 12 })
        ));
    }

    #[test]
    fn a_bundle_for_a_newer_release_is_refused() {
        let b = sign_bundle(&key(), 11, 6, 0, &payload()).unwrap();
        assert!(matches!(
            accept_bundle(&b, &key().verifying_key(), POLICY),
            Err(L10nError::BundleTooNew {
                needed: 6,
                current: 5
            })
        ));
    }

    #[test]
    fn a_signed_non_payload_is_refused() {
        let b = sign_bundle(&key(), 11, 5, 0, b"not json").unwrap();
        assert!(matches!(
            accept_bundle(&b, &key().verifying_key(), POLICY),
            Err(L10nError::BundleMalformed(_))
        ));
    }

    #[test]
    fn short_and_oversized_input_is_refused_before_parsing() {
        let vk = key().verifying_key();
        assert!(accept_bundle(&[], &vk, POLICY).is_err());
        assert!(accept_bundle(&[1u8; HEADER_LEN + SIGNATURE_LEN - 1], &vk, POLICY).is_err());
        assert!(accept_bundle(&vec![1u8; MAX_BUNDLE_BYTES + 1], &vk, POLICY).is_err());
        let mut lying_len = sign_bundle(&key(), 11, 5, 0, &payload()).unwrap();
        lying_len[25..29].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(accept_bundle(&lying_len, &vk, POLICY).is_err());
    }
}
