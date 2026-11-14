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
//!     rendezvous pseudonym + the device's own ADR-0080 device-card
//!     envelope (optional) into the wire-format payload that the
//!     QR-render library turns into pixels, or that the ADR-0049 code
//!     route seals.
//!
//!   * [`decode_pairing_qr`] — confirming side. Parse the wire-format
//!     bytes, verify the embedded login handle matches the handle
//!     of the local account's own username (constant time), return
//!     the KeyPackage bytes + rendezvous pseudonym for the
//!     downstream `addDeviceFromWire` + DS-mailbox PUT flow, and the
//!     new device's card, decoded, to name it
//!     (`docs/ux/second-device.md` section 2.1).
//!
//! See `docs/adr/0007-multi-device-identity.md` and the
//! pairing-flow narrative in
//! `schemas/wire/device_pairing_v1.proto`.

use lakko_contacts::device_card::{decode_device_card, DEVICE_CARD_ENVELOPE_MAX_LEN};
use lakko_mls::error::Error as MlsError;
use lakko_mls::pairing as inner;
use wasm_bindgen::prelude::*;

use crate::device_card::WasmDeviceCard;

// The pairing payload's card bound is the device-card envelope bound;
// `lakko-mls` restates it because it does not depend on `lakko-contacts`.
const _: () = assert!(inner::PAIRING_DEVICE_CARD_MAX_LEN == DEVICE_CARD_ENVELOPE_MAX_LEN);

/// Build the wire-format pairing payload.
///
/// `login_handle` is the 32-byte handle of the username the person
/// typed (`loginHandle(username)`). `key_package` is the TLS-encoded
/// `KeyPackage` from the joining device's DeviceIdentity.
/// `rendezvous_pseudonym` is a fresh 32-byte random slot the joining
/// device chose for this pairing session — the DS mailbox at that
/// pseudonym is the one-shot Welcome delivery channel.
/// `device_card_envelope` is this device's own ADR-0080 device-card
/// envelope (`composeDeviceCardEnvelope`), or `undefined` / `null` for none;
/// it is carried as given for the confirming device to show.
///
/// # Errors
///
/// `JsError` on wrong-length `login_handle` (≠ 32) /
/// `rendezvous_pseudonym` (≠ 32), a `device_card_envelope` longer than the
/// device-card envelope bound (270 bytes), or any of the inner
/// encode failure modes (all unreachable in practice given
/// valid inputs).
#[wasm_bindgen(js_name = encodePairingQr)]
pub fn encode_pairing_qr(
    login_handle: &[u8],
    key_package: &[u8],
    rendezvous_pseudonym: &[u8],
    device_card_envelope: Option<Vec<u8>>,
) -> Result<Vec<u8>, JsError> {
    encode_inner(
        login_handle,
        key_package,
        rendezvous_pseudonym,
        device_card_envelope.as_deref().unwrap_or_default(),
    )
    .map_err(|e| JsError::new(&e))
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
    device_card: &[u8],
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
    inner::encode(&handle, key_package, &rendezvous, device_card)
        .map_err(|e: MlsError| e.to_string())
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

    /// ADR-0081 amendment — on a DeviceGroup rejoin knock, the rejoining
    /// device's self-removal proof (137 bytes, not verified here), else
    /// `undefined`.
    #[wasm_bindgen(getter, js_name = selfRemovalProof)]
    pub fn self_removal_proof(&self) -> Option<Vec<u8>> {
        self.inner.self_removal_proof.clone()
    }

    /// The new device's ADR-0080 device card, decoded, when the payload
    /// carries one that decodes; else `undefined` (always on a rejoin knock,
    /// and for a card that does not decode — never an error). Display only:
    /// it is what the new device says about itself and nothing trusts it.
    #[wasm_bindgen(getter, js_name = deviceCard)]
    pub fn device_card(&self) -> Option<WasmDeviceCard> {
        decoded_card(&self.inner.device_card)
    }
}

/// The card, decoded; `None` for an empty field or one that does not decode.
fn decoded_card(envelope: &[u8]) -> Option<WasmDeviceCard> {
    if envelope.is_empty() {
        return None;
    }
    decode_device_card(envelope).ok().map(Into::into)
}

/// ADR-0081 amendment — a DeviceGroup rejoin knock: the pairing payload plus,
/// when the journal names an epoch, the device's self-removal proof
/// (`signDeviceGroupSelfRemoval`, exactly 137 bytes) for `keyPackage`; an
/// empty proof for none.
///
/// **Sealed (G-55).** `accountIdentity` is the account identity the
/// DeviceGroup id derives from (the one `deviceGroupIdFor` takes: the oldest
/// own predecessor after a rotation, else the current one); the row is sealed
/// under a key derived from its seed and padded, so the Delivery Service reads
/// neither the login handle nor the KeyPackage. Open it with
/// [`open_rejoin_knock`].
///
/// # Errors
///
/// `JsError` on a wrong-length handle, rendezvous pseudonym or proof.
#[wasm_bindgen(js_name = encodeRejoinKnock)]
pub fn encode_rejoin_knock(
    account_identity: &crate::mls::Identity,
    login_handle: &[u8],
    key_package: &[u8],
    rendezvous_pseudonym: &[u8],
    self_removal_proof: &[u8],
) -> Result<Vec<u8>, JsError> {
    let handle: inner::LoginHandle = login_handle.try_into().map_err(|_| {
        JsError::new(&format!(
            "login_handle must be {} bytes",
            inner::LOGIN_HANDLE_LEN
        ))
    })?;
    let rendezvous: inner::RendezvousPseudonym = rendezvous_pseudonym.try_into().map_err(|_| {
        JsError::new(&format!(
            "rendezvous_pseudonym must be {} bytes",
            inner::RENDEZVOUS_PSEUDONYM_LEN
        ))
    })?;
    let seed = account_identity.mls_identity().ed25519_seed_bytes();
    inner::encode_rejoin_knock(&seed, &handle, key_package, &rendezvous, self_removal_proof)
        .map_err(|e| JsError::new(&e.to_string()))
}

/// A sibling's side of [`encode_rejoin_knock`] (G-55): open a row read from
/// the account's DeviceGroup knock queue under `accountIdentity` (the same
/// identity `deviceGroupIdFor` takes), then decode the pairing payload bound
/// to `expectedLoginHandle`.
///
/// # Errors
///
/// `JsError` when the row is not a sealed knock, does not open under this
/// account's key, or its payload does not decode or names another handle.
#[wasm_bindgen(js_name = openRejoinKnock)]
pub fn open_rejoin_knock(
    account_identity: &crate::mls::Identity,
    row: &[u8],
    expected_login_handle: &[u8],
) -> Result<DecodedPairingQr, JsError> {
    let handle: inner::LoginHandle = expected_login_handle.try_into().map_err(|_| {
        JsError::new(&format!(
            "expected_login_handle must be {} bytes",
            inner::LOGIN_HANDLE_LEN
        ))
    })?;
    let seed = account_identity.mls_identity().ed25519_seed_bytes();
    inner::open_rejoin_knock(&seed, row, &handle)
        .map(|inner| DecodedPairingQr { inner })
        .map_err(|e| JsError::new(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G-55: a knock built here opens under the same lineage identity and
    /// carries no login handle in the clear. (A refusal builds a JS error,
    /// which a host test cannot construct; the refusals are pinned in
    /// lakko-mls and on the UniFFI surface.)
    #[test]
    fn a_rejoin_knock_round_trips_under_its_lineage_identity() {
        let lineage = crate::mls::Identity::from_seed(&[0x21; 64]).unwrap();
        let handle = [0x11u8; 32];
        let row = encode_rejoin_knock(&lineage, &handle, &[7u8; 64], &[9u8; 32], &[]).unwrap();
        assert!(!row.windows(32).any(|w| w == handle.as_slice()));
        let got = open_rejoin_knock(&lineage, &row, &handle).unwrap();
        assert_eq!(got.key_package(), vec![7u8; 64]);
        assert_eq!(got.self_removal_proof(), None);
    }

    #[test]
    fn encode_then_decode_round_trips_under_the_same_handle() {
        let wire = encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32], &[]).unwrap();
        let got = decode_inner(&wire, &[0x11; 32]).unwrap();
        assert_eq!(got.key_package, vec![7u8; 64]);
        assert_eq!(got.rendezvous_pseudonym, [9u8; 32]);
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
        use lakko_contacts::device_card::{compose_device_card, DeviceCard, DevicePlatform};
        let card = DeviceCard {
            updated_at_ms: 10,
            last_active_ms: 9,
            platform: DevicePlatform::Web,
            model: "Chrome on macOS".to_string(),
            app_version: "1.4".to_string(),
            os: "macOS 26".to_string(),
            city: "Almere".to_string(),
            country: "NL".to_string(),
        };
        let envelope = compose_device_card(&card).unwrap();
        let wire = encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32], &envelope).unwrap();
        let got = DecodedPairingQr {
            inner: decode_inner(&wire, &[0x11; 32]).unwrap(),
        };
        assert_eq!(got.device_card(), Some(WasmDeviceCard::from(card)));
    }

    #[test]
    fn no_card_and_an_undecodable_card_both_read_as_none() {
        let wire = encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32], &[]).unwrap();
        let got = DecodedPairingQr {
            inner: decode_inner(&wire, &[0x11; 32]).unwrap(),
        };
        assert_eq!(got.device_card(), None);
        let wire = encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32], b"not a card").unwrap();
        let got = DecodedPairingQr {
            inner: decode_inner(&wire, &[0x11; 32]).unwrap(),
        };
        assert_eq!(got.device_card(), None);
        assert_eq!(got.key_package(), vec![7u8; 64]);
    }

    #[test]
    fn an_over_long_card_is_refused_at_encode() {
        let over = vec![0u8; DEVICE_CARD_ENVELOPE_MAX_LEN + 1];
        assert!(encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32], &over).is_err());
    }

    #[test]
    fn decode_refuses_another_handle() {
        let wire = encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32], &[]).unwrap();
        let err = decode_inner(&wire, &[0x12; 32]).unwrap_err();
        assert!(err.contains("login_handle mismatch"), "got: {err}");
    }

    #[test]
    fn wrong_length_handles_are_refused_at_the_boundary() {
        // A 16-byte value (an account id, the v1 shape) is not a handle.
        assert!(encode_inner(&[0x11; 16], &[7u8; 64], &[9u8; 32], &[]).is_err());
        let wire = encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 32], &[]).unwrap();
        assert!(decode_inner(&wire, &[0x11; 16]).is_err());
        assert!(encode_inner(&[0x11; 32], &[7u8; 64], &[9u8; 31], &[]).is_err());
    }
}
