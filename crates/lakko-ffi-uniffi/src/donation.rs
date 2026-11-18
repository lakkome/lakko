// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko-attest`'s donation statements
//! (`docs/ux/donations.md` section 3), the iOS + Android mirror of
//! `crates/lakko-ffi-wasm/src/donation.rs`. Verify only: statements are signed
//! by the offline `lakko-donations` tool, and the native donation screens check
//! each one against the donations key pinned into the app build before showing
//! it.
//!
//! - [`verify_donation_method`] (`verifyDonationMethod`) returns a
//!   [`DonationMethod`] record the screen renders a card from;
//! - [`verify_donation_phone`] (`verifyDonationPhone`) returns the phone number
//!   of the method it was asked for.
//!
//! Every failure is [`FfiError::Donation`] whose message is
//! `"<code>: <reason>"`, the code one of `bad-key` (the pinned key is not an
//! Ed25519 key), `bad-length`, `bad-version`, `bad-kind`, `bad-field`,
//! `kind-mismatch`, `bad-signature`, `id-mismatch` — the same codes the wasm
//! surface throws. The client shows nothing for a statement that fails.

use lakko_attest::{DonationError, DonationKind as InnerKind};
use lakko_crypto::signing::VerifyingKey;

use crate::FfiError;

/// The code a pinned key that is not an Ed25519 key reports.
const BAD_KEY: &str = "bad-key";

fn coded(code: &str, reason: &str) -> FfiError {
    FfiError::Donation(format!("{code}: {reason}"))
}

impl From<DonationError> for FfiError {
    fn from(e: DonationError) -> Self {
        coded(e.code(), &e.to_string())
    }
}

fn key(bytes: &[u8]) -> Result<VerifyingKey, FfiError> {
    VerifyingKey::from_bytes(bytes)
        .map_err(|_| coded(BAD_KEY, "donations key is not a valid Ed25519 key"))
}

/// The kind of a donation method; the screen picks a card's description line
/// from it, so the set is closed.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DonationKind {
    /// SBP, a Russian bank transfer.
    Sbp,
    /// Revolut.
    Revolut,
    /// Buy Me a Coffee.
    Bmc,
    /// A crypto wallet.
    Crypto,
}

impl From<InnerKind> for DonationKind {
    fn from(k: InnerKind) -> Self {
        match k {
            InnerKind::Sbp => Self::Sbp,
            InnerKind::Revolut => Self::Revolut,
            InnerKind::Bmc => Self::Bmc,
            InnerKind::Crypto => Self::Crypto,
        }
    }
}

/// A verified donation method. Every field is part of a statement signed by
/// the donations key; none is secret.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct DonationMethod {
    /// The stable short id (`sbp`, `btc`, ...).
    pub id: String,
    /// The kind.
    pub kind: DonationKind,
    /// The payment link (`https://` only), absent for a crypto method and
    /// optional otherwise.
    pub link: Option<String>,
    /// The coin of a crypto method.
    pub coin: Option<String>,
    /// The network of a crypto method.
    pub network: Option<String>,
    /// The address of a crypto method.
    pub address: Option<String>,
    /// SHA-256 of the QR PNG the server serves for this method (32 bytes);
    /// absent when there is no QR. The client hashes the fetched image and
    /// shows it only when the two are equal.
    pub qr_sha256: Option<Vec<u8>>,
    /// Whether a phone number exists (fetched separately and verified with
    /// [`verify_donation_phone`]).
    pub has_phone: bool,
    /// When the statement was signed, Unix milliseconds.
    pub signed_at_millis: i64,
}

impl From<lakko_attest::DonationMethod> for DonationMethod {
    fn from(m: lakko_attest::DonationMethod) -> Self {
        Self {
            id: m.id,
            kind: m.kind.into(),
            link: m.link,
            coin: m.coin,
            network: m.network,
            address: m.address,
            qr_sha256: m.qr_sha256.map(|h| h.to_vec()),
            has_phone: m.has_phone,
            signed_at_millis: m.signed_at_unix_millis,
        }
    }
}

/// Verify a method statement (`statement`, the decoded `statement_b64` of
/// `GET /discovery/donate`) against the pinned donations key (`key`, 32
/// bytes).
///
/// # Errors
///
/// [`FfiError::Donation`] with a `"<code>: <reason>"` message on any failure.
#[uniffi::export]
pub fn verify_donation_method(
    statement: Vec<u8>,
    key: Vec<u8>,
) -> Result<DonationMethod, FfiError> {
    let vk = self::key(&key)?;
    Ok(lakko_attest::verify_donation_method(&statement, &vk)?.into())
}

/// Verify a phone statement (the decoded `statement_b64` of
/// `POST /discovery/donate/{id}/phone`) against the pinned donations key and
/// the id of the method the number was asked for; returns the number.
///
/// # Errors
///
/// [`FfiError::Donation`] with a `"<code>: <reason>"` message on any failure
/// (`id-mismatch` when the statement names another method).
#[uniffi::export]
pub fn verify_donation_phone(
    statement: Vec<u8>,
    key: Vec<u8>,
    id: String,
) -> Result<String, FfiError> {
    let vk = self::key(&key)?;
    Ok(lakko_attest::verify_donation_phone(&statement, &vk, &id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    fn crypto_method() -> lakko_attest::DonationMethod {
        lakko_attest::DonationMethod {
            signed_at_unix_millis: 5,
            id: "btc".to_string(),
            kind: InnerKind::Crypto,
            link: None,
            coin: Some("Bitcoin".to_string()),
            network: Some("Bitcoin".to_string()),
            address: Some("bc1qexample".to_string()),
            qr_sha256: None,
            has_phone: false,
        }
    }

    fn code(e: &FfiError) -> String {
        match e {
            FfiError::Donation(m) => m.split(':').next().unwrap_or_default().to_string(),
            other => panic!("not a donation error: {other:?}"),
        }
    }

    #[test]
    fn a_signed_crypto_method_verifies_and_projects() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let wire = crypto_method().sign(&sk).unwrap();
        let got = verify_donation_method(wire, sk.verifying_key().to_bytes().to_vec()).unwrap();
        assert_eq!(got.id, "btc");
        assert_eq!(got.kind, DonationKind::Crypto);
        assert_eq!(got.coin.as_deref(), Some("Bitcoin"));
        assert_eq!(got.network.as_deref(), Some("Bitcoin"));
        assert_eq!(got.address.as_deref(), Some("bc1qexample"));
        assert_eq!(got.link, None);
        assert_eq!(got.qr_sha256, None);
        assert!(!got.has_phone);
        assert_eq!(got.signed_at_millis, 5);
    }

    #[test]
    fn a_link_method_carries_its_qr_hash_and_phone_flag() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let method = lakko_attest::DonationMethod {
            signed_at_unix_millis: 7,
            id: "sbp".to_string(),
            kind: InnerKind::Sbp,
            link: Some("https://example.org/pay".to_string()),
            coin: None,
            network: None,
            address: None,
            qr_sha256: Some([0xAB; 32]),
            has_phone: true,
        };
        let wire = method.sign(&sk).unwrap();
        let got = verify_donation_method(wire, sk.verifying_key().to_bytes().to_vec()).unwrap();
        assert_eq!(got.kind, DonationKind::Sbp);
        assert_eq!(got.link.as_deref(), Some("https://example.org/pay"));
        assert_eq!(got.qr_sha256, Some(vec![0xAB; 32]));
        assert!(got.has_phone);
    }

    #[test]
    fn every_kind_projects_one_to_one() {
        for (inner, outer) in [
            (InnerKind::Sbp, DonationKind::Sbp),
            (InnerKind::Revolut, DonationKind::Revolut),
            (InnerKind::Bmc, DonationKind::Bmc),
            (InnerKind::Crypto, DonationKind::Crypto),
        ] {
            assert_eq!(DonationKind::from(inner), outer);
        }
    }

    #[test]
    fn failures_carry_the_core_codes() {
        let sk = SigningKey::from_seed(&[0x11; 32]);
        let vk = sk.verifying_key().to_bytes().to_vec();
        let wire = crypto_method().sign(&sk).unwrap();
        assert_eq!(
            code(&verify_donation_method(wire.clone(), vec![0; 8]).unwrap_err()),
            "bad-key"
        );
        let other = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        assert_eq!(
            code(&verify_donation_method(wire.clone(), other).unwrap_err()),
            "bad-signature"
        );
        assert_eq!(
            code(&verify_donation_method(wire[..10].to_vec(), vk.clone()).unwrap_err()),
            "bad-length"
        );

        let phone = lakko_attest::sign_donation_phone("sbp", "+7 900", &sk).unwrap();
        assert_eq!(
            verify_donation_phone(phone.clone(), vk.clone(), "sbp".to_string()).unwrap(),
            "+7 900"
        );
        assert_eq!(
            code(&verify_donation_phone(phone.clone(), vk, "btc".to_string()).unwrap_err()),
            "id-mismatch"
        );
        assert_eq!(
            code(&verify_donation_phone(phone, vec![1; 31], "sbp".to_string()).unwrap_err()),
            "bad-key"
        );
    }
}
