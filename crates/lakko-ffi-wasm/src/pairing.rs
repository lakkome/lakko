// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the device-pairing payload codec
//! (Phase 2a3b1b; v2 since ADR-0056).
//!
//! Mirror of the UniFFI surface (`lakko-ffi-uniffi::pairing`) and
//! the Rust core (`lakko_mls::pairing`). Two free functions:
//!
//!   * [`encode_pairing_qr`] — fresh-device side. Bundle the
//!     32-byte login handle of the username the person typed
//!     (`loginHandle(username)`) + KeyPackage bytes + 32-byte
//!     rendezvous pseudonym into the wire-format payload that the
//!     QR-render library turns into pixels, or that
//!     `POST /device/request` carries.
//!
//!   * [`decode_pairing_qr`] — confirming side. Parse the wire-format
//!     bytes, verify the embedded login handle matches the handle
//!     of the local account's own username (constant time), return
//!     the KeyPackage bytes + rendezvous pseudonym for the
//!     downstream `addDeviceFromWire` + DS-mailbox PUT flow.
//!
//! See `docs/adr/0007-multi-device-identity.md` and the
//! pairing-flow narrative in
//! `schemas/wire/device_pairing_v1.proto`.

use lakko_mls::error::Error as MlsError;
use lakko_mls::pairing as inner;
use wasm_bindgen::prelude::*;

/// Build the wire-format pairing payload.
///
/// `login_handle` is the 32-byte handle of the username the person
/// typed (`loginHandle(username)`). `key_package` is the TLS-encoded
/// `KeyPackage` from the joining device's DeviceIdentity.
/// `rendezvous_pseudonym` is a fresh 32-byte random slot the joining
/// device chose for this pairing session — the DS mailbox at that
/// pseudonym is the one-shot Welcome delivery channel.
///
/// # Errors
///
/// `JsError` on wrong-length `login_handle` (≠ 32) /
/// `rendezvous_pseudonym` (≠ 32), or any of the inner
/// encode failure modes (all unreachable in practice given
/// valid inputs).
#[wasm_bindgen(js_name = encodePairingQr)]
pub fn encode_pairing_qr(
    login_handle: &[u8],
    key_package: &[u8],
    rendezvous_pseudonym: &[u8],
) -> Result<Vec<u8>, JsError> {
    encode_inner(login_handle, key_package, rendezvous_pseudonym).map_err(|e| JsError::new(&e))
}

/// Confirming side: parse the wire bytes, verify the embedded login
/// handle matches `expected_login_handle` (the handle of the local
/// account's own username), return a structured [`DecodedPairingQr`]
/// handle.
///
/// # Errors
///
/// `JsError` on:
///   - Wrong-length `expected_login_handle` (≠ 32).
///   - Malformed wire bytes (proto-decode failure, wrong
///     embedded field lengths).
///   - A handle mismatch — the payload was made for another
///     account's name.
///   - Any payload version other than 2 (a v1 payload is refused).
#[wasm_bindgen(js_name = decodePairingQr)]
pub fn decode_pairing_qr(
    bytes: &[u8],
    expected_login_handle: &[u8],
) -> Result<DecodedPairingQr, JsError> {
    decode_inner(bytes, expected_login_handle)
        .map(|inner| DecodedPairingQr { inner })
        .map_err(|e| JsError::new(&e))
}

fn encode_inner(
    login_handle: &[u8],
    key_package: &[u8],
    rendezvous_pseudonym: &[u8],
) -> Result<Vec<u8>, String> {
    let handle: inner::LoginHandle = login_handle
        .try_into()
        .map_err(|_| format!("login_handle must be {} bytes", inner::LOGIN_HANDLE_LEN))?;
    let rendezvous: inner::RendezvousPseudonym = rendezvous_pseudonym.try_into().map_err(|_| {
        format!(
            "rendezvous_pseudonym must be {} bytes",
            inner::RENDEZVOUS_PSEUDONYM_LEN
        )
    })?;
    inner::encode(&handle, key_package, &rendezvous).map_err(|e: MlsError| e.to_string())
}

fn decode_inner(
    bytes: &[u8],
    expected_login_handle: &[u8],
) -> Result<inner::DecodedPairingQr, String> {
    let handle: inner::LoginHandle = expected_login_handle.try_into().map_err(|_| {
        format!(
            "expected_login_handle must be {} bytes",
            inner::LOGIN_HANDLE_LEN
        )
    })?;
    inner::decode(bytes, &handle).map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_decode_round_trips_under_the_same_handle() {
        let wire = encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32]).unwrap();
        let got = decode_inner(&wire, &[0x11; 32]).unwrap();
        assert_eq!(got.key_package, vec![7u8; 64]);
        assert_eq!(got.rendezvous_pseudonym, [9u8; 32]);
    }

    #[test]
    fn decode_refuses_another_handle() {
        let wire = encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32]).unwrap();
        let err = decode_inner(&wire, &[0x12; 32]).unwrap_err();
        assert!(err.contains("login_handle mismatch"), "got: {err}");
    }

    #[test]
    fn wrong_length_handles_are_refused_at_the_boundary() {
        // A 16-byte value (an account id, the v1 shape) is not a handle.
        assert!(encode_inner(&[0x11; 16], &[7u8; 64], &[9u8; 32]).is_err());
        let wire = encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32]).unwrap();
        assert!(decode_inner(&wire, &[0x11; 16]).is_err());
        assert!(encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 31]).is_err());
    }
}
