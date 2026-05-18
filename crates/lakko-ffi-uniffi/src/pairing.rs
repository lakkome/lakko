// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the device-pairing QR codec
//! (Phase 2a3b1b).
//!
//! Mirror of `crates/lakko-ffi-wasm/src/pairing.rs` for iOS +
//! Android. Two free functions:
//!
//!   * [`build_pairing_qr`] — fresh-device side. Bundle the
//!     account_id + KeyPackage bytes + 32-byte rendezvous
//!     pseudonym into the wire-format payload the platform's
//!     QR-render library turns into pixels.
//!
//!   * [`parse_pairing_qr`] — scanner side. Parse the wire
//!     bytes the camera library handed us, verify the embedded
//!     account_id matches the local session, return the
//!     KeyPackage bytes + rendezvous pseudonym for the
//!     downstream `addDeviceFromWire` + DS-mailbox PUT flow.
//!
//! `Record` (vs `Object` handle) for the decode result keeps
//! the boundary cheap — both byte arrays cross as `Data` /
//! `ByteArray` and the lifetime ends at return.

use lakko_mls::pairing as inner;

use crate::FfiError;

// `From<lakko_mls::Error> for FfiError` is already defined at
// the crate root (`lib.rs`) — no duplicate impl here.

/// Decoded pairing-QR contents the scanner side acts on.
#[derive(uniffi::Record, Clone)]
pub struct DecodedPairingQr {
    /// TLS-encoded `KeyPackage` for the joining device.
    pub key_package: Vec<u8>,
    /// 32-byte rendezvous pseudonym for the DS-mailbox Welcome
    /// delivery slot.
    pub rendezvous_pseudonym: Vec<u8>,
}

/// Build the wire-format pairing QR payload.
///
/// # Errors
///
/// * [`FfiError::Codec`] on wrong-length `account_id` (≠ 16)
///   / `rendezvous_pseudonym` (≠ 32).
/// * [`FfiError::Mls`] on the (practically unreachable) inner
///   encode failure.
#[uniffi::export]
pub fn build_pairing_qr(
    account_id: Vec<u8>,
    key_package: Vec<u8>,
    rendezvous_pseudonym: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    if account_id.len() != 16 {
        return Err(FfiError::Codec("account_id must be 16 bytes".into()));
    }
    if rendezvous_pseudonym.len() != inner::RENDEZVOUS_PSEUDONYM_LEN {
        return Err(FfiError::Codec(format!(
            "rendezvous_pseudonym must be {} bytes",
            inner::RENDEZVOUS_PSEUDONYM_LEN,
        )));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(&account_id);
    let mut rendezvous = [0u8; inner::RENDEZVOUS_PSEUDONYM_LEN];
    rendezvous.copy_from_slice(&rendezvous_pseudonym);

    Ok(inner::encode(&acct, &key_package, &rendezvous)?)
}

/// Scanner side: parse the wire bytes, verify the embedded
/// `account_id` matches `expected_account_id`, return the
/// decoded contents.
///
/// # Errors
///
/// See the docstring of [`build_pairing_qr`] — same error
/// vocabulary plus the wire-format-rejection cases from
/// [`lakko_mls::pairing::decode`].
#[uniffi::export]
pub fn parse_pairing_qr(
    wire: Vec<u8>,
    expected_account_id: Vec<u8>,
) -> Result<DecodedPairingQr, FfiError> {
    if expected_account_id.len() != 16 {
        return Err(FfiError::Codec(
            "expected_account_id must be 16 bytes".into(),
        ));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(&expected_account_id);
    let decoded = inner::decode(&wire, &acct)?;
    Ok(DecodedPairingQr {
        key_package: decoded.key_package,
        rendezvous_pseudonym: decoded.rendezvous_pseudonym.to_vec(),
    })
}
