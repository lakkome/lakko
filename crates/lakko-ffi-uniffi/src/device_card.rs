// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the device card (ADR-0080): what a device tells the
//! account's other devices about itself.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/device_card.rs` for iOS + Android.
//! The card is sent ONLY in the account's DeviceGroup (as a
//! `serviceEnvelope` message). A receiver classifies it with
//! `dispatchIncomingEnvelope` (kind `DeviceCard`), stores it under the
//! sender's DeviceGroup leaf signature key resolved at receipt (C-68, never a
//! leaf index), keeps it only when [`device_card_is_newer`], and drops it when
//! the leaf leaves the DeviceGroup.

use lakko_contacts::device_card::{
    compose_device_card as rust_compose, decode_device_card as rust_decode,
    device_card_is_newer as rust_is_newer, DeviceCard, DeviceCardError, DevicePlatform,
    DEVICE_CARD_ENVELOPE_MAX_LEN, DEVICE_CARD_ENVELOPE_MIN_LEN,
};

use crate::FfiError;

// A maximal card must fit the DeviceGroup `serviceEnvelope` carrier.
const _: () = assert!(DEVICE_CARD_ENVELOPE_MAX_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN);

fn wire_error(e: DeviceCardError) -> FfiError {
    FfiError::Contacts(format!("device card {}: {e}", e.code()))
}

/// The closed set of platforms a card may name (ADR-0080).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiDevicePlatform {
    /// iPhone.
    Ios,
    /// iPad.
    Ipados,
    /// A Mac running the native app.
    Macos,
    /// Android phone or tablet.
    Android,
    /// The web client in a browser.
    Web,
    /// A desktop app other than the native Mac app.
    Desktop,
}

impl From<DevicePlatform> for FfiDevicePlatform {
    fn from(p: DevicePlatform) -> Self {
        match p {
            DevicePlatform::Ios => Self::Ios,
            DevicePlatform::Ipados => Self::Ipados,
            DevicePlatform::Macos => Self::Macos,
            DevicePlatform::Android => Self::Android,
            DevicePlatform::Web => Self::Web,
            DevicePlatform::Desktop => Self::Desktop,
        }
    }
}

impl From<FfiDevicePlatform> for DevicePlatform {
    fn from(p: FfiDevicePlatform) -> Self {
        match p {
            FfiDevicePlatform::Ios => Self::Ios,
            FfiDevicePlatform::Ipados => Self::Ipados,
            FfiDevicePlatform::Macos => Self::Macos,
            FfiDevicePlatform::Android => Self::Android,
            FfiDevicePlatform::Web => Self::Web,
            FfiDevicePlatform::Desktop => Self::Desktop,
        }
    }
}

/// A device card. Mirror of [`lakko_contacts::device_card::DeviceCard`].
/// Every field is the sending device's own statement about itself.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiDeviceCard {
    /// Sender-clock UNIX milliseconds at which the card was made; the order
    /// key. Re-emit verbatim on a growth re-send.
    pub updated_at_ms: u64,
    /// Sender-clock UNIX milliseconds of the person's last action on the
    /// sending device.
    pub last_active_ms: u64,
    /// The platform.
    pub platform: FfiDevicePlatform,
    /// Model or browser name, e.g. "iPhone 16 Pro", "Chrome on macOS".
    pub model: String,
    /// App version, e.g. "1.4".
    pub app_version: String,
    /// System name and version, e.g. "iOS 27.0.1".
    pub os: String,
    /// City of the device's internet address, or empty.
    pub city: String,
    /// Two-letter country code of that city, or empty.
    pub country: String,
}

impl From<DeviceCard> for FfiDeviceCard {
    fn from(c: DeviceCard) -> Self {
        Self {
            updated_at_ms: c.updated_at_ms,
            last_active_ms: c.last_active_ms,
            platform: c.platform.into(),
            model: c.model,
            app_version: c.app_version,
            os: c.os,
            city: c.city,
            country: c.country,
        }
    }
}

impl From<FfiDeviceCard> for DeviceCard {
    fn from(c: FfiDeviceCard) -> Self {
        Self {
            updated_at_ms: c.updated_at_ms,
            last_active_ms: c.last_active_ms,
            platform: c.platform.into(),
            model: c.model,
            app_version: c.app_version,
            os: c.os,
            city: c.city,
            country: c.country,
        }
    }
}

/// Compose a device-card envelope: the bytes to hand to
/// `DeviceGroup.createServiceEnvelopeMessage`.
///
/// The card is normalized first ([`normalize_device_card`]): text taken from
/// the operating system never makes this fail.
///
/// # Errors
///
/// [`FfiError::Contacts`] — unreachable after normalization; kept so a bound
/// change cannot turn into a panic.
#[uniffi::export]
pub fn compose_device_card_envelope(card: FfiDeviceCard) -> Result<Vec<u8>, FfiError> {
    let card = DeviceCard::from(card).normalized();
    rust_compose(&card).map_err(wire_error)
}

/// The card with every string made admissible: refused characters removed,
/// whitespace trimmed, each string cut to its byte bound, a country that is
/// not two ASCII letters emptied.
#[uniffi::export]
#[must_use]
pub fn normalize_device_card(card: FfiDeviceCard) -> FfiDeviceCard {
    DeviceCard::from(card).normalized().into()
}

/// Strictly decode a device-card envelope. Fail-closed.
///
/// # Errors
///
/// [`FfiError::Contacts`] carrying the frozen reason code.
#[uniffi::export]
pub fn decode_device_card_envelope(plaintext: Vec<u8>) -> Result<FfiDeviceCard, FfiError> {
    rust_decode(&plaintext).map(Into::into).map_err(wire_error)
}

/// `true` when a card stamped `incoming_updated_at_ms` must replace the one
/// stored for the same leaf key. Strictly greater wins.
#[uniffi::export]
#[must_use]
pub fn device_card_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Shortest device-card envelope.
#[uniffi::export]
#[must_use]
pub fn device_card_envelope_min_len() -> u32 {
    u32::try_from(DEVICE_CARD_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// Longest device-card envelope; below the DeviceGroup service-envelope cap.
#[uniffi::export]
#[must_use]
pub fn device_card_envelope_max_len() -> u32 {
    u32::try_from(DEVICE_CARD_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> FfiDeviceCard {
        FfiDeviceCard {
            updated_at_ms: 10,
            last_active_ms: 9,
            platform: FfiDevicePlatform::Android,
            model: "  Pixel 7a\n".to_string(),
            app_version: "1.4".to_string(),
            os: "Android 15".to_string(),
            city: "Almere".to_string(),
            country: "nl".to_string(),
        }
    }

    #[test]
    fn compose_normalizes_and_round_trips_through_dispatch() {
        let wire = compose_device_card_envelope(card()).unwrap();
        let decoded = decode_device_card_envelope(wire.clone()).unwrap();
        assert_eq!(decoded.model, "Pixel 7a");
        assert_eq!(decoded.country, "NL");
        assert_eq!(decoded, normalize_device_card(card()));
        let dispatched = crate::contacts::dispatch_incoming_envelope(wire).unwrap();
        assert_eq!(
            dispatched.kind,
            crate::contacts::FfiIncomingEnvelopeKind::DeviceCard
        );
        assert_eq!(dispatched.device_card, Some(decoded));
        assert!(dispatched.payload.is_empty());
    }

    #[test]
    fn the_card_fits_the_device_group_carrier() {
        assert_eq!(device_card_envelope_min_len(), 44);
        assert!(!device_card_is_newer(5, 5));
        assert!(decode_device_card_envelope(b"nope".to_vec()).is_err());
    }
}
