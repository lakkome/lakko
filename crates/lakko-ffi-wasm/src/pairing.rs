// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the device-pairing QR codec
//! (Phase 2a3b1b).
//!
//! Mirror of the UniFFI surface (`lakko-ffi-uniffi::pairing`) and
//! the Rust core (`lakko_mls::pairing`). Two free functions:
//!
//!   * [`encode_pairing_qr`] — fresh-device side. Bundle the
//!     account_id + KeyPackage bytes + 32-byte rendezvous
//!     pseudonym into the wire-format payload that the QR-render
//!     library on the JS side then turns into pixels.
//!
//!   * [`decode_pairing_qr`] — scanner side. Parse the wire-format
//!     bytes the camera library handed us, verify the embedded
//!     account_id matches the local session, return the
//!     KeyPackage bytes + rendezvous pseudonym for the
//!     downstream `addDeviceFromWire` + DS-mailbox PUT flow.
//!
//! See `docs/adr/0007-multi-device-identity.md` and the
//! pairing-flow narrative in
//! `schemas/wire/device_pairing_v1.proto`.

use lakko_mls::error::Error as MlsError;
use lakko_mls::pairing as inner;
use wasm_bindgen::prelude::*;

/// Build the wire-format pairing QR payload.
///
/// `account_id` is the 16-byte UUIDv7. `key_package` is the
/// TLS-encoded `KeyPackage` from the joining device's
/// DeviceIdentity. `rendezvous_pseudonym` is a fresh 32-byte
/// random slot the joining device chose for this pairing
/// session — the DS mailbox at that pseudonym is the one-shot
/// Welcome delivery channel.
///
/// # Errors
///
/// `JsError` on wrong-length `account_id` (≠ 16) /
/// `rendezvous_pseudonym` (≠ 32), or any of the inner
/// encode failure modes (all unreachable in practice given
/// valid inputs).
#[wasm_bindgen(js_name = encodePairingQr)]
pub fn encode_pairing_qr(
    account_id: &[u8],
    key_package: &[u8],
    rendezvous_pseudonym: &[u8],
) -> Result<Vec<u8>, JsError> {
    if account_id.len() != 16 {
        return Err(JsError::new("account_id must be 16 bytes"));
    }
    if rendezvous_pseudonym.len() != inner::RENDEZVOUS_PSEUDONYM_LEN {
        return Err(JsError::new(&format!(
            "rendezvous_pseudonym must be {} bytes",
            inner::RENDEZVOUS_PSEUDONYM_LEN,
        )));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(account_id);
    let mut rendezvous = [0u8; inner::RENDEZVOUS_PSEUDONYM_LEN];
    rendezvous.copy_from_slice(rendezvous_pseudonym);

    inner::encode(&acct, key_package, &rendezvous).map_err(err_to_js)
}

/// Scanner side: parse the wire bytes, verify the embedded
/// `account_id` matches `expected_account_id`, return a
/// structured [`DecodedPairingQr`] handle.
///
/// # Errors
///
/// `JsError` on:
///   - Wrong-length `expected_account_id` (≠ 16).
///   - Malformed wire bytes (proto-decode failure, wrong
///     embedded field lengths).
///   - `expected_account_id` mismatch — the QR was scanned
///     against a session for a different account.
///   - Newer-version payload than this binary recognises.
#[wasm_bindgen(js_name = decodePairingQr)]
pub fn decode_pairing_qr(
    bytes: &[u8],
    expected_account_id: &[u8],
) -> Result<DecodedPairingQr, JsError> {
    if expected_account_id.len() != 16 {
        return Err(JsError::new("expected_account_id must be 16 bytes"));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(expected_account_id);
    let decoded = inner::decode(bytes, &acct).map_err(err_to_js)?;
    Ok(DecodedPairingQr { inner: decoded })
}

/// Decoded pairing QR — returned by [`decode_pairing_qr`].
/// Exposes the two byte arrays the scanner side needs to drive
/// the add-device flow: the joining device's KeyPackage (feed
/// to `DeviceGroup.addDeviceFromWire`) and the rendezvous
/// pseudonym (the DS mailbox slot to PUT the Welcome at).
#[wasm_bindgen]
pub struct DecodedPairingQr {
    inner: inner::DecodedPairingQr,
}

#[wasm_bindgen]
impl DecodedPairingQr {
    /// TLS-encoded `KeyPackage` for the joining device.
    #[wasm_bindgen(getter, js_name = keyPackage)]
    pub fn key_package(&self) -> Vec<u8> {
        self.inner.key_package.clone()
    }

    /// 32-byte rendezvous pseudonym for the DS-mailbox
    /// Welcome delivery slot.
    #[wasm_bindgen(getter, js_name = rendezvousPseudonym)]
    pub fn rendezvous_pseudonym(&self) -> Vec<u8> {
        self.inner.rendezvous_pseudonym.to_vec()
    }
}

fn err_to_js(e: MlsError) -> JsError {
    JsError::new(&e.to_string())
}
