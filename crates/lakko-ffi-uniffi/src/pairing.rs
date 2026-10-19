// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the device-pairing payload codec
//! (Phase 2a3b1b; v2 since ADR-0056).
//!
//! Mirror of `crates/lakko-ffi-wasm/src/pairing.rs` for iOS +
//! Android. Two free functions:
//!
//!   * [`build_pairing_qr`] — fresh-device side. Bundle the
//!     32-byte login handle of the username the person typed
//!     (`loginHandle(username)`) + KeyPackage bytes + 32-byte
//!     rendezvous pseudonym into the wire-format payload the
//!     platform's QR-render library turns into pixels, or that
//!     `POST /device/request` carries.
//!
//!   * [`parse_pairing_qr`] — confirming side. Parse the wire
//!     bytes, verify the embedded login handle matches the
//!     handle of the local account's own username (constant
//!     time), return the KeyPackage bytes + rendezvous pseudonym
//!     for the downstream `addDeviceFromWire` + DS-mailbox PUT
//!     flow.
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
    /// ADR-0081 amendment — on a DeviceGroup rejoin knock, the rejoining
    /// device's self-removal proof (137 bytes, not verified here: the
    /// admitting sibling carries it with the replacement commit and every
    /// peer's removal gate checks it). `None` on a pairing QR.
    #[uniffi(default = None)]
    pub self_removal_proof: Option<Vec<u8>>,
}

/// Build the wire-format pairing payload.
///
/// # Errors
///
/// * [`FfiError::Codec`] on wrong-length `login_handle` (≠ 32)
///   / `rendezvous_pseudonym` (≠ 32).
/// * [`FfiError::Codec`] also on the (practically unreachable) inner
///   encode failure.
#[uniffi::export]
pub fn build_pairing_qr(
    login_handle: Vec<u8>,
    key_package: Vec<u8>,
    rendezvous_pseudonym: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let handle = handle_array(&login_handle, "login_handle")?;
    let rendezvous: inner::RendezvousPseudonym =
        rendezvous_pseudonym.as_slice().try_into().map_err(|_| {
            FfiError::Codec(format!(
                "rendezvous_pseudonym must be {} bytes",
                inner::RENDEZVOUS_PSEUDONYM_LEN,
            ))
        })?;
    Ok(inner::encode(&handle, &key_package, &rendezvous)?)
}

/// ADR-0081 amendment — a DeviceGroup rejoin knock: the pairing payload plus
/// the device's self-removal proof (`signDeviceGroupSelfRemoval`, exactly 137
/// bytes) for `key_package`.
///
/// # Errors
///
/// [`FfiError::Codec`] on a wrong-length handle, rendezvous pseudonym or
/// proof.
#[uniffi::export]
pub fn build_rejoin_knock(
    login_handle: Vec<u8>,
    key_package: Vec<u8>,
    rendezvous_pseudonym: Vec<u8>,
    self_removal_proof: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let handle = handle_array(&login_handle, "login_handle")?;
    let rendezvous: inner::RendezvousPseudonym =
        rendezvous_pseudonym.as_slice().try_into().map_err(|_| {
            FfiError::Codec(format!(
                "rendezvous_pseudonym must be {} bytes",
                inner::RENDEZVOUS_PSEUDONYM_LEN,
            ))
        })?;
    Ok(inner::encode_rejoin_knock(
        &handle,
        &key_package,
        &rendezvous,
        &self_removal_proof,
    )?)
}

/// Confirming side: parse the wire bytes, verify the embedded
/// login handle matches `expected_login_handle` (the handle of
/// the local account's own username), return the decoded
/// contents.
///
/// # Errors
///
/// * [`FfiError::Codec`] on a wrong-length `expected_login_handle`
///   or a malformed payload (bad protobuf, wrong embedded lengths).
/// * [`FfiError::Unsupported`] on any payload version other than 2
///   (a v1 payload from a client that has not moved is refused).
/// * [`FfiError::Group`] on a handle mismatch — the payload was made
///   for another account's name.
#[uniffi::export]
pub fn parse_pairing_qr(
    wire: Vec<u8>,
    expected_login_handle: Vec<u8>,
) -> Result<DecodedPairingQr, FfiError> {
    let handle = handle_array(&expected_login_handle, "expected_login_handle")?;
    let decoded = inner::decode(&wire, &handle)?;
    Ok(DecodedPairingQr {
        key_package: decoded.key_package,
        rendezvous_pseudonym: decoded.rendezvous_pseudonym.to_vec(),
        self_removal_proof: decoded.self_removal_proof,
    })
}

fn handle_array(bytes: &[u8], what: &str) -> Result<inner::LoginHandle, FfiError> {
    bytes
        .try_into()
        .map_err(|_| FfiError::Codec(format!("{what} must be {} bytes", inner::LOGIN_HANDLE_LEN)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_then_parse_round_trips_under_the_same_handle() {
        let handle = vec![0x11u8; 32];
        let wire = build_pairing_qr(handle.clone(), vec![7u8; 64], vec![9u8; 32]).unwrap();
        let got = parse_pairing_qr(wire, handle).unwrap();
        assert_eq!(got.key_package, vec![7u8; 64]);
        assert_eq!(got.rendezvous_pseudonym, vec![9u8; 32]);
    }

    #[test]
    fn parse_refuses_another_handle() {
        let wire = build_pairing_qr(vec![0x11; 32], vec![7u8; 64], vec![9u8; 32]).unwrap();
        assert!(matches!(
            parse_pairing_qr(wire, vec![0x12; 32]),
            Err(FfiError::Group(_))
        ));
    }

    #[test]
    fn wrong_length_handles_are_refused_at_the_boundary() {
        // A 16-byte value (an account id, the v1 shape) is not a handle.
        assert!(matches!(
            build_pairing_qr(vec![0x11; 16], vec![7u8; 64], vec![9u8; 32]),
            Err(FfiError::Codec(_))
        ));
        let wire = build_pairing_qr(vec![0x11; 32], vec![7u8; 64], vec![9u8; 32]).unwrap();
        assert!(matches!(
            parse_pairing_qr(wire, vec![0x11; 16]),
            Err(FfiError::Codec(_))
        ));
        assert!(matches!(
            build_pairing_qr(vec![0x11; 32], vec![7u8; 64], vec![9u8; 31]),
            Err(FfiError::Codec(_))
        ));
    }
}
