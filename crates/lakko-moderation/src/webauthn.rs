// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The check of a moderator's WebAuthn assertion ([ADR-0089] section 3).
//!
//! A moderator signs a statement by touching their security key: the panel
//! asks for an assertion whose challenge is the statement's 32-byte hash.
//! A service verifies it against the key the root-signed list holds:
//!
//! 1. client data: `type` is `webauthn.get`, `challenge` is the base64url of
//!    the expected hash, `origin` is the list's origin, `crossOrigin` is not
//!    true;
//! 2. authenticator data: the RP id hash is SHA-256 of the list's RP id,
//!    user presence and user verification are both set;
//! 3. the signature over `authenticatorData ‖ SHA-256(clientDataJSON)` under
//!    the listed key (Ed25519, or P-256 ECDSA with SHA-256).
//!
//! This is deliberately not the vendored `webauthn-rs-core`: that fork is
//! pinned to auth's own ceremonies (ADR-0014), and a service here only checks
//! one assertion against one known key. The signature counter is not
//! checked: a cloned security key is outside what this check can see.
//!
//! [ADR-0089]: ../../../docs/adr/0089-reporting-and-moderation.md

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash::sha256;
use lakko_crypto::signing::{Signature, VerifyingKey};
use p256::ecdsa::signature::Verifier as _;
use serde::Deserialize;

use crate::moderators::{ModeratorKey, ModeratorList};
use crate::ModerationError;

/// The longest client data accepted.
pub const MAX_CLIENT_DATA: usize = 4096;
/// The longest authenticator data accepted.
pub const MAX_AUTHENTICATOR_DATA: usize = 1024;
/// The longest signature accepted (a DER P-256 signature is at most 72).
pub const MAX_SIGNATURE: usize = 80;

const FLAG_USER_PRESENT: u8 = 0x01;
const FLAG_USER_VERIFIED: u8 = 0x04;

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(rename = "crossOrigin", default)]
    cross_origin: Option<bool>,
}

/// One assertion as the browser returned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    /// `authenticatorData`.
    pub authenticator_data: Vec<u8>,
    /// `clientDataJSON`.
    pub client_data_json: Vec<u8>,
    /// `signature`.
    pub signature: Vec<u8>,
}

/// Verify `assertion` as a signature over `challenge` by the listed `key`.
///
/// # Errors
///
/// [`ModerationError::Assertion`] naming the failed check, or
/// [`ModerationError::BadSignature`].
pub fn verify_assertion(
    list: &ModeratorList,
    key: &ModeratorKey,
    challenge: &[u8; 32],
    assertion: &Assertion,
) -> Result<(), ModerationError> {
    let cdj = &assertion.client_data_json;
    if cdj.len() > MAX_CLIENT_DATA {
        return Err(ModerationError::Assertion("client data too long"));
    }
    let data: ClientData =
        serde_json::from_slice(cdj).map_err(|_| ModerationError::Assertion("client data"))?;
    if data.kind != "webauthn.get" {
        return Err(ModerationError::Assertion("type"));
    }
    if data.challenge != URL_SAFE_NO_PAD.encode(challenge) {
        return Err(ModerationError::Assertion("challenge"));
    }
    if data.origin != list.origin {
        return Err(ModerationError::Assertion("origin"));
    }
    if data.cross_origin == Some(true) {
        return Err(ModerationError::Assertion("cross origin"));
    }
    let ad = &assertion.authenticator_data;
    if ad.len() < 37 || ad.len() > MAX_AUTHENTICATOR_DATA {
        return Err(ModerationError::Assertion("authenticator data length"));
    }
    if ad[..32] != sha256(list.rp_id.as_bytes()) {
        return Err(ModerationError::Assertion("rp id"));
    }
    if ad[32] & FLAG_USER_PRESENT == 0 || ad[32] & FLAG_USER_VERIFIED == 0 {
        return Err(ModerationError::Assertion("user presence or verification"));
    }
    if assertion.signature.len() > MAX_SIGNATURE {
        return Err(ModerationError::BadSignature);
    }
    let mut message = Vec::with_capacity(ad.len() + 32);
    message.extend_from_slice(ad);
    message.extend_from_slice(&sha256(cdj));
    match key {
        ModeratorKey::Ed25519(k) => {
            let vk = VerifyingKey::from_bytes(k).map_err(|_| ModerationError::BadSignature)?;
            let sig = Signature::from_bytes(&assertion.signature)
                .map_err(|_| ModerationError::BadSignature)?;
            vk.verify(&message, &sig)
                .map_err(|_| ModerationError::BadSignature)
        },
        ModeratorKey::P256(k) => {
            let vk = p256::ecdsa::VerifyingKey::from_sec1_bytes(k)
                .map_err(|_| ModerationError::BadSignature)?;
            let sig = p256::ecdsa::Signature::from_der(&assertion.signature)
                .map_err(|_| ModerationError::BadSignature)?;
            vk.verify(&message, &sig)
                .map_err(|_| ModerationError::BadSignature)
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::moderators::tests::list;
    use lakko_crypto::signing::SigningKey;
    use p256::ecdsa::signature::Signer as _;

    pub(crate) fn p256_key() -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_bytes(&[11u8; 32].into()).unwrap()
    }

    pub(crate) fn p256_public() -> [u8; 65] {
        let point = p256_key().verifying_key().to_encoded_point(false);
        point.as_bytes().try_into().unwrap()
    }

    pub(crate) fn ed_seed() -> [u8; 32] {
        [12u8; 32]
    }

    pub(crate) fn make(
        list: &ModeratorList,
        key: &ModeratorKey,
        challenge: &[u8; 32],
        flags: u8,
    ) -> Assertion {
        let cdj = format!(
            r#"{{"type":"webauthn.get","challenge":"{}","origin":"{}","crossOrigin":false}}"#,
            URL_SAFE_NO_PAD.encode(challenge),
            list.origin
        )
        .into_bytes();
        let mut ad = sha256(list.rp_id.as_bytes()).to_vec();
        ad.push(flags);
        ad.extend_from_slice(&[0, 0, 0, 7]);
        let mut msg = ad.clone();
        msg.extend_from_slice(&sha256(&cdj));
        let signature = match key {
            ModeratorKey::Ed25519(_) => SigningKey::from_seed(&ed_seed())
                .sign(&msg)
                .to_bytes()
                .to_vec(),
            ModeratorKey::P256(_) => {
                let sig: p256::ecdsa::Signature = p256_key().sign(&msg);
                sig.to_der().as_bytes().to_vec()
            },
        };
        Assertion {
            authenticator_data: ad,
            client_data_json: cdj,
            signature,
        }
    }

    pub(crate) fn keys() -> [ModeratorKey; 2] {
        [
            ModeratorKey::Ed25519(SigningKey::from_seed(&ed_seed()).verifying_key().to_bytes()),
            ModeratorKey::P256(p256_public()),
        ]
    }

    #[test]
    fn both_algorithms_verify() {
        let l = list();
        for key in keys() {
            let a = make(&l, &key, &[5; 32], 0x05);
            assert_eq!(verify_assertion(&l, &key, &[5; 32], &a), Ok(()));
            assert_eq!(
                verify_assertion(&l, &key, &[6; 32], &a),
                Err(ModerationError::Assertion("challenge"))
            );
        }
    }

    #[test]
    fn every_check_refuses() {
        let l = list();
        for key in keys() {
            let a = make(&l, &key, &[5; 32], 0x01);
            assert!(verify_assertion(&l, &key, &[5; 32], &a).is_err(), "no UV");
            let mut other = l.clone();
            other.origin = "https://evil.example".into();
            let a = make(&other, &key, &[5; 32], 0x05);
            assert_eq!(
                verify_assertion(&l, &key, &[5; 32], &a),
                Err(ModerationError::Assertion("origin"))
            );
            let mut other = l.clone();
            other.rp_id = "evil.example".into();
            let a = make(&other, &key, &[5; 32], 0x05);
            let mut a2 = a.clone();
            a2.client_data_json = make(&l, &key, &[5; 32], 0x05).client_data_json;
            assert!(verify_assertion(&l, &key, &[5; 32], &a2).is_err());
            let mut a = make(&l, &key, &[5; 32], 0x05);
            a.authenticator_data[33] ^= 1;
            assert_eq!(
                verify_assertion(&l, &key, &[5; 32], &a),
                Err(ModerationError::BadSignature)
            );
        }
        let [ed, p] = keys();
        let a = make(&l, &ed, &[5; 32], 0x05);
        assert!(verify_assertion(&l, &p, &[5; 32], &a).is_err());
    }

    #[test]
    fn client_data_must_be_a_get() {
        let l = list();
        let [ed, _] = keys();
        let mut a = make(&l, &ed, &[5; 32], 0x05);
        a.client_data_json = String::from_utf8(a.client_data_json)
            .unwrap()
            .replace("webauthn.get", "webauthn.create")
            .into_bytes();
        assert_eq!(
            verify_assertion(&l, &ed, &[5; 32], &a),
            Err(ModerationError::Assertion("type"))
        );
    }
}
