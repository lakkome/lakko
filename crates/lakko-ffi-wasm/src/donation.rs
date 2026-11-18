// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko-attest`'s donation statements
//! (`docs/ux/donations.md` section 3). Verify only: statements are signed by
//! the offline `lakko-donations` tool, and the web client checks each one
//! against the donations key pinned into its build before showing it.
//!
//! - [`verify_donation_method`] (`verifyDonationMethod`) returns a
//!   [`DonationMethod`] whose getters the donations page reads;
//! - [`verify_donation_phone`] (`verifyDonationPhone`) returns the phone
//!   number of the method it was asked for.
//!
//! Every failure throws an `Error` carrying a stable `donationError` code
//! property: `bad-key` (the pinned key is not an Ed25519 key), `bad-length`,
//! `bad-version`, `bad-kind`, `bad-field`, `kind-mismatch`, `bad-signature`,
//! `id-mismatch`. The client shows nothing for a statement that throws.
//! The UniFFI twin is `crates/lakko-ffi-uniffi/src/donation.rs` (iOS, Android).

use lakko_attest::DonationError;
use lakko_crypto::signing::VerifyingKey;
use wasm_bindgen::prelude::*;

use crate::contacts::coded_error;

/// The property a thrown donation error carries its code under.
const ERROR_PROPERTY: &str = "donationError";

/// Why a donation statement was not shown: a statement error, or a pinned
/// key that is not a key.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    BadKey,
    Statement(DonationError),
}

impl Failure {
    const fn code(&self) -> &'static str {
        match self {
            Self::BadKey => "bad-key",
            Self::Statement(e) => e.code(),
        }
    }

    fn message(&self) -> String {
        match self {
            Self::BadKey => "donations key is not a valid Ed25519 key".to_string(),
            Self::Statement(e) => e.to_string(),
        }
    }

    fn into_js(self) -> JsValue {
        coded_error(&self.message(), ERROR_PROPERTY, self.code())
    }
}

fn key(bytes: &[u8]) -> Result<VerifyingKey, Failure> {
    VerifyingKey::from_bytes(bytes).map_err(|_| Failure::BadKey)
}

/// A verified donation method.
#[wasm_bindgen]
pub struct DonationMethod {
    inner: lakko_attest::DonationMethod,
}

#[wasm_bindgen]
impl DonationMethod {
    /// The stable short id (`sbp`, `btc`, ...).
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.inner.id.clone()
    }

    /// `"sbp"`, `"revolut"`, `"bmc"` or `"crypto"`.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.inner.kind.as_str().to_string()
    }

    /// The payment link (`https://` only), absent for a crypto method and
    /// optional otherwise.
    #[wasm_bindgen(getter)]
    pub fn link(&self) -> Option<String> {
        self.inner.link.clone()
    }

    /// The coin of a crypto method.
    #[wasm_bindgen(getter)]
    pub fn coin(&self) -> Option<String> {
        self.inner.coin.clone()
    }

    /// The network of a crypto method.
    #[wasm_bindgen(getter)]
    pub fn network(&self) -> Option<String> {
        self.inner.network.clone()
    }

    /// The address of a crypto method.
    #[wasm_bindgen(getter)]
    pub fn address(&self) -> Option<String> {
        self.inner.address.clone()
    }

    /// SHA-256 of the QR PNG the server serves for this method (32 bytes);
    /// absent when there is no QR. The client hashes the fetched image and
    /// shows it only when the two are equal.
    #[wasm_bindgen(getter, js_name = qrSha256)]
    pub fn qr_sha256(&self) -> Option<Vec<u8>> {
        self.inner.qr_sha256.map(|h| h.to_vec())
    }

    /// Whether a phone number exists (fetched separately and verified with
    /// [`verify_donation_phone`]).
    #[wasm_bindgen(getter, js_name = hasPhone)]
    pub fn has_phone(&self) -> bool {
        self.inner.has_phone
    }

    /// When the statement was signed, Unix milliseconds (BigInt).
    #[wasm_bindgen(getter, js_name = signedAtMillis)]
    pub fn signed_at_millis(&self) -> i64 {
        self.inner.signed_at_unix_millis
    }
}

/// Verify a method statement (`statement`, the decoded `statement_b64` of
/// `GET /discovery/donate`) against the pinned donations key (`key`, 32
/// bytes). Throws an `Error` with a `donationError` code on any failure.
#[wasm_bindgen(js_name = verifyDonationMethod)]
pub fn verify_donation_method(statement: &[u8], key: &[u8]) -> Result<DonationMethod, JsValue> {
    verify_method_inner(statement, key)
        .map(|inner| DonationMethod { inner })
        .map_err(Failure::into_js)
}

fn verify_method_inner(
    statement: &[u8],
    key_bytes: &[u8],
) -> Result<lakko_attest::DonationMethod, Failure> {
    lakko_attest::verify_donation_method(statement, &key(key_bytes)?).map_err(Failure::Statement)
}

/// Verify a phone statement (the decoded `statement_b64` of
/// `POST /discovery/donate/{id}/phone`) against the pinned donations key and
/// the id of the method the number was asked for; returns the number. Throws
/// an `Error` with a `donationError` code on any failure (`id-mismatch` when
/// the statement names another method).
#[wasm_bindgen(js_name = verifyDonationPhone)]
pub fn verify_donation_phone(statement: &[u8], key: &[u8], id: &str) -> Result<String, JsValue> {
    verify_phone_inner(statement, key, id).map_err(Failure::into_js)
}

fn verify_phone_inner(statement: &[u8], key_bytes: &[u8], id: &str) -> Result<String, Failure> {
    lakko_attest::verify_donation_phone(statement, &key(key_bytes)?, id).map_err(Failure::Statement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_attest::DonationKind;
    use lakko_crypto::signing::SigningKey;

    fn method() -> lakko_attest::DonationMethod {
        lakko_attest::DonationMethod {
            signed_at_unix_millis: 5,
            id: "btc".to_string(),
            kind: DonationKind::Crypto,
            link: None,
            coin: Some("Bitcoin".to_string()),
            network: Some("Bitcoin".to_string()),
            address: Some("bc1qexample".to_string()),
            qr_sha256: None,
            has_phone: false,
        }
    }

    #[test]
    fn a_signed_method_verifies_and_projects() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let wire = method().sign(&sk).unwrap();
        let inner = verify_method_inner(&wire, &sk.verifying_key().to_bytes()).unwrap();
        let view = DonationMethod { inner };
        assert_eq!(view.id(), "btc");
        assert_eq!(view.kind(), "crypto");
        assert_eq!(view.address().as_deref(), Some("bc1qexample"));
        assert_eq!(view.link(), None);
        assert_eq!(view.qr_sha256(), None);
        assert!(!view.has_phone());
        assert_eq!(view.signed_at_millis(), 5);
    }

    #[test]
    fn failures_carry_their_codes() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let vk = sk.verifying_key().to_bytes();
        let wire = method().sign(&sk).unwrap();
        assert_eq!(
            verify_method_inner(&wire, &[0; 8]).unwrap_err().code(),
            "bad-key"
        );
        let other = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_method_inner(&wire, &other).unwrap_err().code(),
            "bad-signature"
        );
        assert_eq!(
            verify_method_inner(&wire[..10], &vk).unwrap_err().code(),
            "bad-length"
        );
        let phone = lakko_attest::sign_donation_phone("sbp", "+7 900", &sk).unwrap();
        assert_eq!(verify_phone_inner(&phone, &vk, "sbp").unwrap(), "+7 900");
        assert_eq!(
            verify_phone_inner(&phone, &vk, "btc").unwrap_err().code(),
            "id-mismatch"
        );
    }
}
