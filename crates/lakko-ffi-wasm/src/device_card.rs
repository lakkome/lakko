// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the device card (ADR-0080): what a device tells
//! the account's other devices about itself.
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/device_card.rs` for the web client.
//! The card is sent ONLY in the account's DeviceGroup (as a
//! `serviceEnvelope` message). A receiver classifies it with
//! `dispatchIncomingEnvelope` (kind `DeviceCard`), stores it under the
//! sender's DeviceGroup leaf signature key resolved at receipt (C-68, never a
//! leaf index), keeps it only when `deviceCardIsNewer`, and drops it when the
//! leaf leaves the DeviceGroup.
//!
//! The platform crosses as its stable name: `ios`, `ipados`, `macos`,
//! `android`, `web`, `desktop`.
//!
//! # Errors
//!
//! Every function throws a string `"<code>: <reason>"`; the codes are the
//! frozen `DeviceCardError::code` values plus `unknownPlatform` for a
//! platform name outside the set.

use lakko_contacts::device_card::{
    compose_device_card as rust_compose, decode_device_card as rust_decode,
    device_card_is_newer as rust_is_newer, DeviceCard as RustDeviceCard, DeviceCardError,
    DevicePlatform, DEVICE_CARD_ENVELOPE_MAX_LEN, DEVICE_CARD_ENVELOPE_MIN_LEN,
};
use wasm_bindgen::prelude::*;

// A maximal card must fit the DeviceGroup `serviceEnvelope` carrier.
const _: () = assert!(DEVICE_CARD_ENVELOPE_MAX_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN);

fn wire_error(e: DeviceCardError) -> String {
    format!("{}: {e}", e.code())
}

/// A device card. Every field is the sending device's own statement.
#[wasm_bindgen(js_name = DeviceCard)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmDeviceCard {
    inner: RustDeviceCard,
}

impl From<RustDeviceCard> for WasmDeviceCard {
    fn from(inner: RustDeviceCard) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen(js_class = DeviceCard)]
impl WasmDeviceCard {
    /// Build a card. `platform` is a stable name; `updatedAtMs` and
    /// `lastActiveMs` are `bigint` UNIX milliseconds. Strings are taken as
    /// given; `composeDeviceCardEnvelope` normalizes them.
    ///
    /// # Errors
    ///
    /// `"unknownPlatform: ..."` for a platform name outside the set.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        updated_at_ms: u64,
        last_active_ms: u64,
        platform: &str,
        model: String,
        app_version: String,
        os: String,
        city: String,
        country: String,
    ) -> Result<Self, String> {
        let platform = DevicePlatform::from_name(platform)
            .ok_or_else(|| "unknownPlatform: not a device platform".to_string())?;
        Ok(Self {
            inner: RustDeviceCard {
                updated_at_ms,
                last_active_ms,
                platform,
                model,
                app_version,
                os,
                city,
                country,
            },
        })
    }

    /// Sender-clock UNIX milliseconds of the card; the order key.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }

    /// Sender-clock UNIX milliseconds of the person's last action there.
    #[wasm_bindgen(getter, js_name = lastActiveMs)]
    pub fn last_active_ms(&self) -> u64 {
        self.inner.last_active_ms
    }

    /// The platform's stable name.
    #[wasm_bindgen(getter)]
    pub fn platform(&self) -> String {
        self.inner.platform.name().to_string()
    }

    /// Model or browser name.
    #[wasm_bindgen(getter)]
    pub fn model(&self) -> String {
        self.inner.model.clone()
    }

    /// App version.
    #[wasm_bindgen(getter, js_name = appVersion)]
    pub fn app_version(&self) -> String {
        self.inner.app_version.clone()
    }

    /// System name and version.
    #[wasm_bindgen(getter)]
    pub fn os(&self) -> String {
        self.inner.os.clone()
    }

    /// City, or empty.
    #[wasm_bindgen(getter)]
    pub fn city(&self) -> String {
        self.inner.city.clone()
    }

    /// Two-letter country code, or empty.
    #[wasm_bindgen(getter)]
    pub fn country(&self) -> String {
        self.inner.country.clone()
    }

    /// The same card with every string made admissible.
    #[must_use]
    pub fn normalized(&self) -> Self {
        self.inner.normalized().into()
    }
}

/// Compose a device-card envelope: the bytes to hand to
/// `DeviceGroup.createServiceEnvelopeMessage`. Normalizes first, so text
/// taken from the browser never makes this fail.
///
/// # Errors
///
/// A wire error string; unreachable after normalization.
#[wasm_bindgen(js_name = composeDeviceCardEnvelope)]
pub fn compose_device_card_envelope(card: &WasmDeviceCard) -> Result<Vec<u8>, String> {
    rust_compose(&card.inner.normalized()).map_err(wire_error)
}

/// Strictly decode a device-card envelope. Fail-closed.
///
/// # Errors
///
/// `"<code>: <reason>"`.
#[wasm_bindgen(js_name = decodeDeviceCardEnvelope)]
pub fn decode_device_card_envelope(plaintext: &[u8]) -> Result<WasmDeviceCard, String> {
    rust_decode(plaintext).map(Into::into).map_err(wire_error)
}

/// `true` when a card stamped `incoming` must replace the one stored for the
/// same leaf key. Strictly greater wins.
#[wasm_bindgen(js_name = deviceCardIsNewer)]
#[must_use]
pub fn device_card_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Shortest device-card envelope.
#[wasm_bindgen(js_name = deviceCardEnvelopeMinLen)]
#[must_use]
pub fn device_card_envelope_min_len() -> u32 {
    u32::try_from(DEVICE_CARD_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// Longest device-card envelope.
#[wasm_bindgen(js_name = deviceCardEnvelopeMaxLen)]
#[must_use]
pub fn device_card_envelope_max_len() -> u32 {
    u32::try_from(DEVICE_CARD_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_normalizes_and_decodes() {
        let card = WasmDeviceCard::new(
            3,
            2,
            "web",
            " Chrome on macOS ".to_string(),
            "web".to_string(),
            "macOS 26".to_string(),
            String::new(),
            String::new(),
        )
        .unwrap();
        let wire = compose_device_card_envelope(&card).unwrap();
        let back = decode_device_card_envelope(&wire).unwrap();
        assert_eq!(back.model(), "Chrome on macOS");
        assert_eq!(back.platform(), "web");
        assert_eq!(back, card.normalized());
        assert!(WasmDeviceCard::new(
            0,
            0,
            "windows",
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new()
        )
        .is_err());
        let err = decode_device_card_envelope(b"x").unwrap_err();
        assert!(err.starts_with("wrongDomain:"));
    }
}
