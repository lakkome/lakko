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
//!     rendezvous pseudonym + the device's own ADR-0080 device-card
//!     envelope (optional) into the wire-format payload the
//!     platform's QR-render library turns into pixels, or that the
//!     ADR-0049 code route seals.
//!
//!   * [`parse_pairing_qr`] — confirming side. Parse the wire
//!     bytes, verify the embedded login handle matches the
//!     handle of the local account's own username (constant
//!     time), return the KeyPackage bytes + rendezvous pseudonym
//!     for the downstream `addDeviceFromWire` + DS-mailbox PUT
//!     flow, and the new device's card, decoded, to name it
//!     (`docs/ux/second-device.md` section 2.1).
//!
//! `Record` (vs `Object` handle) for the decode result keeps
//! the boundary cheap — both byte arrays cross as `Data` /
//! `ByteArray` and the lifetime ends at return.

use lakko_contacts::device_card::{decode_device_card, DEVICE_CARD_ENVELOPE_MAX_LEN};
use lakko_mls::pairing as inner;

use crate::device_card::FfiDeviceCard;
use crate::{FfiError, Identity};

// The pairing payload's card bound is the device-card envelope bound;
// `lakko-mls` restates it because it does not depend on `lakko-contacts`.
const _: () = assert!(inner::PAIRING_DEVICE_CARD_MAX_LEN == DEVICE_CARD_ENVELOPE_MAX_LEN);

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
    /// The new device's ADR-0080 device card, decoded, when the payload
    /// carries one that decodes. `None` when absent (always on a rejoin
    /// knock) or when the card does not decode. Display only: it is what the
    /// new device says about itself and nothing trusts it.
    #[uniffi(default = None)]
    pub device_card: Option<FfiDeviceCard>,
}

/// Build the wire-format pairing payload.
///
/// `device_card_envelope` is this device's own ADR-0080 device-card
/// envelope (`composeDeviceCardEnvelope`), or `None` for none. It is carried
/// as given for the confirming device to show.
///
/// # Errors
///
/// * [`FfiError::Codec`] on wrong-length `login_handle` (≠ 32)
///   / `rendezvous_pseudonym` (≠ 32), or a `device_card_envelope` longer
///   than the device-card envelope bound (270 bytes).
/// * [`FfiError::Codec`] also on the (practically unreachable) inner
///   encode failure.
#[uniffi::export(default(device_card_envelope = None))]
pub fn build_pairing_qr(
    login_handle: Vec<u8>,
    key_package: Vec<u8>,
    rendezvous_pseudonym: Vec<u8>,
    device_card_envelope: Option<Vec<u8>>,
) -> Result<Vec<u8>, FfiError> {
    let handle = handle_array(&login_handle, "login_handle")?;
    let rendezvous: inner::RendezvousPseudonym =
        rendezvous_pseudonym.as_slice().try_into().map_err(|_| {
            FfiError::Codec(format!(
                "rendezvous_pseudonym must be {} bytes",
                inner::RENDEZVOUS_PSEUDONYM_LEN,
            ))
        })?;
    Ok(inner::encode(
        &handle,
        &key_package,
        &rendezvous,
        device_card_envelope.as_deref().unwrap_or_default(),
    )?)
}

/// G-56 — the pairing payload of [`build_pairing_qr`] carrying this device's
/// self-removal proof (`signDeviceGroupSelfRemoval`, exactly 137 bytes) for
/// `key_package`; an empty proof is the plain payload. Used by a device that
/// is already in the account's DeviceGroup and pairs again under the same
/// device identity: the confirming device replaces its leaf in one commit
/// (`replaceDeviceAt`) once `deviceGroupSelfRemovalCovers` holds. Not sealed,
/// like every pairing payload.
///
/// # Errors
///
/// [`FfiError::Codec`] on a wrong-length handle, rendezvous pseudonym or
/// proof, or a `device_card_envelope` longer than 270 bytes.
#[uniffi::export(default(device_card_envelope = None))]
pub fn build_pairing_qr_with_self_removal_proof(
    login_handle: Vec<u8>,
    key_package: Vec<u8>,
    rendezvous_pseudonym: Vec<u8>,
    self_removal_proof: Vec<u8>,
    device_card_envelope: Option<Vec<u8>>,
) -> Result<Vec<u8>, FfiError> {
    let handle = handle_array(&login_handle, "login_handle")?;
    let rendezvous: inner::RendezvousPseudonym =
        rendezvous_pseudonym.as_slice().try_into().map_err(|_| {
            FfiError::Codec(format!(
                "rendezvous_pseudonym must be {} bytes",
                inner::RENDEZVOUS_PSEUDONYM_LEN,
            ))
        })?;
    Ok(inner::encode_with_self_removal_proof(
        &handle,
        &key_package,
        &rendezvous,
        device_card_envelope.as_deref().unwrap_or_default(),
        &self_removal_proof,
    )?)
}

/// ADR-0081 amendment — a DeviceGroup rejoin knock: the pairing payload plus,
/// when the journal names an epoch, the device's self-removal proof
/// (`signDeviceGroupSelfRemoval`, exactly 137 bytes) for `key_package`; an
/// empty proof for none.
///
/// **Sealed (G-55).** `account_identity` is the account identity the
/// DeviceGroup id derives from (the one `deviceGroupIdFor` takes: the oldest
/// own predecessor after a rotation, else the current one); the row is sealed
/// under a key derived from its seed and padded, so the Delivery Service reads
/// neither the login handle nor the KeyPackage. Open it with
/// [`open_rejoin_knock`].
///
/// # Errors
///
/// [`FfiError::Codec`] on a wrong-length handle, rendezvous pseudonym or
/// proof.
#[uniffi::export]
pub fn build_rejoin_knock(
    account_identity: &Identity,
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
        &account_identity.inner.ed25519_seed_bytes(),
        &handle,
        &key_package,
        &rendezvous,
        &self_removal_proof,
    )?)
}

/// A sibling's side of [`build_rejoin_knock`] (G-55): open a row read from
/// the account's DeviceGroup knock queue under `account_identity` (the same
/// identity `deviceGroupIdFor` takes), then parse the pairing payload bound
/// to `expected_login_handle` as [`parse_pairing_qr`] does.
///
/// # Errors
///
/// [`FfiError::Codec`] when the row is not a sealed knock or does not open
/// under this account's key; then what [`parse_pairing_qr`] returns.
#[uniffi::export]
pub fn open_rejoin_knock(
    account_identity: &Identity,
    row: Vec<u8>,
    expected_login_handle: Vec<u8>,
) -> Result<DecodedPairingQr, FfiError> {
    let handle = handle_array(&expected_login_handle, "expected_login_handle")?;
    let decoded =
        inner::open_rejoin_knock(&account_identity.inner.ed25519_seed_bytes(), &row, &handle)?;
    Ok(DecodedPairingQr {
        key_package: decoded.key_package,
        rendezvous_pseudonym: decoded.rendezvous_pseudonym.to_vec(),
        self_removal_proof: decoded.self_removal_proof,
        device_card: decoded_card(&decoded.device_card),
    })
}

/// Confirming side: parse the wire bytes, verify the embedded
/// login handle matches `expected_login_handle` (the handle of
/// the local account's own username), return the decoded
/// contents. A device card that does not decode is reported as `None`,
/// never as an error: it only names the device.
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
        device_card: decoded_card(&decoded.device_card),
    })
}

/// The card, decoded; `None` for an empty field or one that does not decode.
fn decoded_card(envelope: &[u8]) -> Option<FfiDeviceCard> {
    if envelope.is_empty() {
        return None;
    }
    decode_device_card(envelope).ok().map(Into::into)
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
        let wire = build_pairing_qr(handle.clone(), vec![7u8; 64], vec![9u8; 32], None).unwrap();
        let got = parse_pairing_qr(wire, handle).unwrap();
        assert_eq!(got.key_package, vec![7u8; 64]);
        assert_eq!(got.rendezvous_pseudonym, vec![9u8; 32]);
    }

    /// G-56 — the plain payload with a self-removal proof parses back with
    /// it; a proof of the wrong length is refused.
    #[test]
    fn a_payload_with_a_self_removal_proof_round_trips() {
        let handle = vec![0x11u8; 32];
        let proof = vec![0x5au8; lakko_mls::SELF_REMOVAL_PROOF_LEN];
        let wire = build_pairing_qr_with_self_removal_proof(
            handle.clone(),
            vec![7u8; 64],
            vec![9u8; 32],
            proof.clone(),
            None,
        )
        .unwrap();
        let got = parse_pairing_qr(wire, handle.clone()).unwrap();
        assert_eq!(got.self_removal_proof, Some(proof));
        assert!(build_pairing_qr_with_self_removal_proof(
            handle,
            vec![7u8; 64],
            vec![9u8; 32],
            vec![0u8; 3],
            None,
        )
        .is_err());
    }

    fn card() -> crate::device_card::FfiDeviceCard {
        crate::device_card::FfiDeviceCard {
            updated_at_ms: 10,
            last_active_ms: 9,
            platform: crate::device_card::FfiDevicePlatform::Web,
            model: "Chrome on macOS".to_string(),
            app_version: "1.4".to_string(),
            os: "macOS 26".to_string(),
            city: "Almere".to_string(),
            country: "NL".to_string(),
        }
    }

    #[test]
    fn the_pairing_card_bound_is_the_device_card_envelope_bound() {
        assert_eq!(
            inner::PAIRING_DEVICE_CARD_MAX_LEN,
            DEVICE_CARD_ENVELOPE_MAX_LEN
        );
    }

    #[test]
    fn a_card_crosses_the_payload_and_is_decoded_on_the_other_side() {
        let envelope = crate::device_card::compose_device_card_envelope(card()).unwrap();
        let wire =
            build_pairing_qr(vec![0x11; 32], vec![7u8; 64], vec![9u8; 32], Some(envelope)).unwrap();
        let got = parse_pairing_qr(wire, vec![0x11; 32]).unwrap();
        assert_eq!(got.device_card, Some(card()));
        assert_eq!(got.self_removal_proof, None);
    }

    #[test]
    fn no_card_and_an_undecodable_card_both_read_as_none() {
        let wire = build_pairing_qr(vec![0x11; 32], vec![7u8; 64], vec![9u8; 32], None).unwrap();
        assert_eq!(
            parse_pairing_qr(wire, vec![0x11; 32]).unwrap().device_card,
            None
        );
        let wire = build_pairing_qr(
            vec![0x11; 32],
            vec![7u8; 64],
            vec![9u8; 32],
            Some(b"not a device card".to_vec()),
        )
        .unwrap();
        let got = parse_pairing_qr(wire, vec![0x11; 32]).unwrap();
        assert_eq!(got.device_card, None);
        assert_eq!(got.key_package, vec![7u8; 64]);
    }

    #[test]
    fn a_rejoin_knock_opens_only_under_its_lineage_identity() {
        let lineage = Identity::from_seed(vec![0x21; 64]).unwrap();
        let other = Identity::from_seed(vec![0x22; 64]).unwrap();
        let handle = vec![0x11u8; 32];
        let row = build_rejoin_knock(
            &lineage,
            handle.clone(),
            vec![7u8; 64],
            vec![9u8; 32],
            vec![],
        )
        .unwrap();
        assert!(
            !row.windows(32).any(|w| w == handle.as_slice()),
            "no handle in the row"
        );
        let got = open_rejoin_knock(&lineage, row.clone(), handle.clone()).unwrap();
        assert_eq!(got.key_package, vec![7u8; 64]);
        assert_eq!(got.self_removal_proof, None);
        assert!(open_rejoin_knock(&other, row.clone(), handle.clone()).is_err());
        assert!(
            parse_pairing_qr(row, handle).is_err(),
            "no plain read of a knock"
        );
    }

    /// The code route seals the payload with `lakko-device-link`, which
    /// refuses a payload above its `MAX_PAYLOAD_LEN`: a real bound device
    /// KeyPackage plus a card at its bound must still fit.
    #[test]
    fn a_payload_with_a_maximal_card_fits_the_code_route() {
        let mut device = lakko_mls::Identity::from_seed(&[0x31; 64]).unwrap();
        device.bind_to_account(&[0x42; 32], 1_700_000_000).unwrap();
        let kp = lakko_mls::key_package::serialize(&device.create_key_package().unwrap()).unwrap();
        let wire = build_pairing_qr(
            vec![0x11; 32],
            kp,
            vec![9u8; 32],
            Some(vec![0x6c; DEVICE_CARD_ENVELOPE_MAX_LEN]),
        )
        .unwrap();
        assert!(
            wire.len() <= lakko_device_link::MAX_PAYLOAD_LEN,
            "payload {} > {}",
            wire.len(),
            lakko_device_link::MAX_PAYLOAD_LEN
        );
    }

    #[test]
    fn an_over_long_card_is_refused_at_build() {
        assert!(matches!(
            build_pairing_qr(
                vec![0x11; 32],
                vec![7u8; 64],
                vec![9u8; 32],
                Some(vec![0u8; DEVICE_CARD_ENVELOPE_MAX_LEN + 1]),
            ),
            Err(FfiError::Codec(_))
        ));
    }

    #[test]
    fn parse_refuses_another_handle() {
        let wire = build_pairing_qr(vec![0x11; 32], vec![7u8; 64], vec![9u8; 32], None).unwrap();
        assert!(matches!(
            parse_pairing_qr(wire, vec![0x12; 32]),
            Err(FfiError::Group(_))
        ));
    }

    #[test]
    fn wrong_length_handles_are_refused_at_the_boundary() {
        // A 16-byte value (an account id, the v1 shape) is not a handle.
        assert!(matches!(
            build_pairing_qr(vec![0x11; 16], vec![7u8; 64], vec![9u8; 32], None),
            Err(FfiError::Codec(_))
        ));
        let wire = build_pairing_qr(vec![0x11; 32], vec![7u8; 64], vec![9u8; 32], None).unwrap();
        assert!(matches!(
            parse_pairing_qr(wire, vec![0x11; 16]),
            Err(FfiError::Codec(_))
        ));
        assert!(matches!(
            build_pairing_qr(vec![0x11; 32], vec![7u8; 64], vec![9u8; 31], None),
            Err(FfiError::Codec(_))
        ));
    }
}
