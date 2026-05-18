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

/// Length of the rendezvous pseudonym in bytes. Pinned at 32
/// to mirror every other pseudonym width in Lakko (routing
/// pseudonym in `wire/v1.proto`, contact pseudonyms in
/// `lakko-contacts`).
pub const RENDEZVOUS_PSEUDONYM_LEN: usize = 32;

/// 32-byte rendezvous pseudonym. The DS mailbox at this slot
/// is the one-shot Welcome delivery channel for an add-device
/// pairing session. Generated freshly by the joining device
/// each pairing — never reused across pairings.
pub type RendezvousPseudonym = [u8; RENDEZVOUS_PSEUDONYM_LEN];

/// Decoded pairing-QR contents the scanner side acts on.
/// Returned by [`decode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPairingQr {
    /// TLS-encoded `KeyPackage` for the joining device's
    /// DeviceIdentity. Hand straight to
    /// [`crate::device_group::DeviceGroup::add_device_from_wire`].
    pub key_package: Vec<u8>,
    /// 32-byte rendezvous pseudonym the scanner posts the
    /// resulting Welcome to via the DS mailbox.
    pub rendezvous_pseudonym: RendezvousPseudonym,
}

/// Fresh-device side: build the wire-format QR payload.
///
/// `key_package_bytes` is the TLS-encoded `KeyPackage` from
/// `Identity::create_key_package()` plus
/// `lakko_mls::key_package::serialize(...)`. Caller owns the
/// bytes — this function does not retain a reference past
/// return.
///
/// `rendezvous_pseudonym` is a fresh random 32-byte slot the
/// joining device chose for this pairing session. The scanner
/// will PUT the Welcome bytes to the DS mailbox keyed by this
/// pseudonym; the joining device polls that slot until the
/// Welcome arrives.
///
/// # Errors
///
/// Returns [`Error::Codec`] on the unlikely prost-encode
/// failure (constant-size fields make it practically
/// impossible).
pub fn encode(
    account_id: &AccountId,
    key_package_bytes: &[u8],
    rendezvous_pseudonym: &RendezvousPseudonym,
) -> Result<Vec<u8>> {
    let payload = lakko_proto::wire::device_pairing::v1::PairingQr {
        version: PAIRING_VERSION,
        account_id: account_id.to_vec(),
        key_package: key_package_bytes.to_vec(),
        rendezvous_pseudonym: rendezvous_pseudonym.to_vec(),
    };
    let mut buf = Vec::with_capacity(payload.encoded_len());
    payload
        .encode(&mut buf)
        .map_err(|e| Error::Codec(format!("PairingQr encode: {e}")))?;
    Ok(buf)
}

/// Scanner side: parse the wire-format QR payload and bind it
/// to `expected_account_id`. Returns the embedded
/// `KeyPackage` bytes + rendezvous pseudonym ready to drive
/// [`crate::device_group::DeviceGroup::add_device_from_wire`]
/// and the subsequent Welcome PUT.
///
/// # Errors
///
/// - [`Error::Codec`] when the bytes don't decode or the
///   embedded `account_id` / `rendezvous_pseudonym` is the
///   wrong length (structurally suspicious).
/// - [`Error::Unsupported`] when the payload's `version` is
///   newer than this binary knows.
/// - [`Error::Group`] with a `"account_id mismatch"` body when
///   the embedded `account_id` is not equal to
///   `expected_account_id`. The scanner refuses to enrol the
///   fresh device into the local DeviceGroup.
pub fn decode(bytes: &[u8], expected_account_id: &AccountId) -> Result<DecodedPairingQr> {
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
    if payload.rendezvous_pseudonym.len() != RENDEZVOUS_PSEUDONYM_LEN {
        return Err(Error::Codec(format!(
            "PairingQr rendezvous_pseudonym length {} != {RENDEZVOUS_PSEUDONYM_LEN}",
            payload.rendezvous_pseudonym.len()
        )));
    }
    let mut rendezvous = [0u8; RENDEZVOUS_PSEUDONYM_LEN];
    rendezvous.copy_from_slice(&payload.rendezvous_pseudonym);
    Ok(DecodedPairingQr {
        key_package: payload.key_package,
        rendezvous_pseudonym: rendezvous,
    })
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

    fn fixture_rendezvous() -> RendezvousPseudonym {
        let mut p = [0u8; RENDEZVOUS_PSEUDONYM_LEN];
        p[0] = 0xfe;
        p[RENDEZVOUS_PSEUDONYM_LEN - 1] = 0xed;
        p
    }

    #[test]
    fn encode_decode_roundtrip() {
        let account = fixture_account_id();
        let kp_bytes = vec![0x42u8; 768]; // shape-realistic stub
        let rendezvous = fixture_rendezvous();
        let qr = encode(&account, &kp_bytes, &rendezvous).expect("encode");
        let recovered = decode(&qr, &account).expect("decode");
        assert_eq!(recovered.key_package, kp_bytes);
        assert_eq!(recovered.rendezvous_pseudonym, rendezvous);
    }

    #[test]
    fn decode_rejects_wrong_account() {
        let account_a = fixture_account_id();
        let mut account_b = fixture_account_id();
        account_b[0] ^= 0xFF;
        let kp_bytes = vec![0x42u8; 128];
        let qr = encode(&account_a, &kp_bytes, &fixture_rendezvous()).expect("encode");
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
            rendezvous_pseudonym: fixture_rendezvous().to_vec(),
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
            rendezvous_pseudonym: fixture_rendezvous().to_vec(),
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

    /// Phase 2a3b — wrong-length rendezvous_pseudonym must be
    /// rejected before any DS-mailbox interaction.
    #[test]
    fn decode_rejects_bad_rendezvous_pseudonym_length() {
        let account = fixture_account_id();
        // 16-byte rendezvous instead of 32.
        let payload = lakko_proto::wire::device_pairing::v1::PairingQr {
            version: PAIRING_VERSION,
            account_id: account.to_vec(),
            key_package: vec![0u8; 32],
            rendezvous_pseudonym: vec![0xee; 16],
        };
        let mut buf = Vec::new();
        payload.encode(&mut buf).unwrap();
        match decode(&buf, &account) {
            Ok(_) => panic!("decode should reject short rendezvous_pseudonym"),
            Err(Error::Codec(msg)) => {
                assert!(
                    msg.contains("rendezvous_pseudonym"),
                    "expected error to mention rendezvous_pseudonym, got: {msg}"
                );
            },
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
}
