// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Device-pairing QR payload (Phase 2a3b1).
//!
//! Wraps the generated [`lakko_proto::wire::device_pairing::v1::PairingQr`]
//! type with two helpers the three clients (web / iOS / Android)
//! consume to drive the add-device handshake:
//!
//!   * [`encode`] — fresh device side. Bundle its
//!     `Identity::create_key_package()` bytes + its
//!     `account_id` into the wire payload to render as a QR.
//!   * [`decode`] — scanner side. Parse the QR bytes, **verify
//!     the embedded `account_id` matches the local account**,
//!     and surface the inner [`KeyPackage`] for
//!     [`DeviceGroup::add_device_from_wire`].
//!
//! The wire format itself lives in
//! `schemas/wire/device_pairing_v1.proto`; this module is the
//! one place that knows the field shape so the three clients
//! don't end up with subtly different decode paths.
//!
//! ## Trust model
//!
//! - The QR payload is **not** signed — see the proto docstring.
//!   The trust anchor is the user pointing the camera at their
//!   own device's screen.
//! - The `account_id` check is the only structural guard: it
//!   stops the scanner from accidentally enrolling a stranger's
//!   device into the local DeviceGroup if they scan a QR from
//!   the wrong session by mistake. It does **not** stop an
//!   active attacker who can present an arbitrary QR — that's
//!   what the safety-number follow-up will cover.

use prost::Message as _;

use crate::device_group::AccountId;
use crate::error::{Error, Result};

/// Current QR payload schema version. Bumped on breaking
/// changes; older scanners reject newer payloads.
pub const PAIRING_VERSION: u32 = 1;

/// Fresh-device side: build the wire-format QR payload.
///
/// `key_package_bytes` is the TLS-encoded `KeyPackage` from
/// `Identity::create_key_package()` plus
/// `lakko_mls::key_package::serialize(...)`. Caller owns the
/// bytes — this function does not retain a reference past
/// return.
///
/// # Errors
///
/// Returns [`Error::Codec`] on the unlikely prost-encode
/// failure (constant-size fields make it practically
/// impossible).
pub fn encode(account_id: &AccountId, key_package_bytes: &[u8]) -> Result<Vec<u8>> {
    let payload = lakko_proto::wire::device_pairing::v1::PairingQr {
        version: PAIRING_VERSION,
        account_id: account_id.to_vec(),
        key_package: key_package_bytes.to_vec(),
    };
    let mut buf = Vec::with_capacity(payload.encoded_len());
    payload
        .encode(&mut buf)
        .map_err(|e| Error::Codec(format!("PairingQr encode: {e}")))?;
    Ok(buf)
}

/// Scanner side: parse the wire-format QR payload and bind it
/// to `expected_account_id`. Returns the embedded
/// `KeyPackage` bytes ready to feed into
/// [`crate::device_group::DeviceGroup::add_device_from_wire`].
///
/// # Errors
///
/// - [`Error::Codec`] when the bytes don't decode or the
///   embedded `account_id` is not 16 bytes (wrong length is
///   structurally suspicious).
/// - [`Error::Unsupported`] when the payload's `version` is
///   newer than this binary knows.
/// - [`Error::Group`] with a `"account_id mismatch"` body when
///   the embedded `account_id` is not equal to
///   `expected_account_id`. The scanner refuses to enrol the
///   fresh device into the local DeviceGroup.
pub fn decode(bytes: &[u8], expected_account_id: &AccountId) -> Result<Vec<u8>> {
    let payload = lakko_proto::wire::device_pairing::v1::PairingQr::decode(bytes)
        .map_err(|e| Error::Codec(format!("PairingQr decode: {e}")))?;

    if payload.version > PAIRING_VERSION {
        return Err(Error::Unsupported("pairing QR is newer than this client"));
    }
    if payload.account_id.len() != 16 {
        return Err(Error::Codec(format!(
            "PairingQr account_id length {} != 16",
            payload.account_id.len()
        )));
    }
    if payload.account_id.as_slice() != expected_account_id.as_slice() {
        return Err(Error::Group(
            "pairing QR account_id mismatch (scanned from a different account)".into(),
        ));
    }
    Ok(payload.key_package)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_account_id() -> AccountId {
        let mut id = [0u8; 16];
        id[0] = 0xab;
        id[15] = 0xcd;
        id
    }

    #[test]
    fn encode_decode_roundtrip() {
        let account = fixture_account_id();
        let kp_bytes = vec![0x42u8; 768]; // shape-realistic stub
        let qr = encode(&account, &kp_bytes).expect("encode");
        let recovered = decode(&qr, &account).expect("decode");
        assert_eq!(recovered, kp_bytes);
    }

    #[test]
    fn decode_rejects_wrong_account() {
        let account_a = fixture_account_id();
        let mut account_b = fixture_account_id();
        account_b[0] ^= 0xFF;
        let kp_bytes = vec![0x42u8; 128];
        let qr = encode(&account_a, &kp_bytes).expect("encode");
        match decode(&qr, &account_b) {
            Ok(_) => panic!("decode should reject a QR for the other account"),
            Err(Error::Group(msg)) => {
                assert!(msg.contains("account_id mismatch"), "got: {msg}");
            },
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_newer_version() {
        let account = fixture_account_id();
        // Hand-roll a payload with version = 999.
        let payload = lakko_proto::wire::device_pairing::v1::PairingQr {
            version: 999,
            account_id: account.to_vec(),
            key_package: vec![0u8; 32],
        };
        let mut buf = Vec::new();
        payload.encode(&mut buf).unwrap();
        match decode(&buf, &account) {
            Ok(_) => panic!("decode should reject a future version"),
            Err(Error::Unsupported(_)) => {},
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_bad_account_id_length() {
        let account = fixture_account_id();
        // 8-byte account_id instead of 16.
        let payload = lakko_proto::wire::device_pairing::v1::PairingQr {
            version: PAIRING_VERSION,
            account_id: vec![0xaa; 8],
            key_package: vec![0u8; 32],
        };
        let mut buf = Vec::new();
        payload.encode(&mut buf).unwrap();
        match decode(&buf, &account) {
            Ok(_) => panic!("decode should reject short account_id"),
            Err(Error::Codec(msg)) => {
                assert!(msg.contains("length"), "got: {msg}");
            },
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
}
