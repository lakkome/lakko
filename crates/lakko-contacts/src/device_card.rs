// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Device card — each device tells the account's other devices what it is
//! ([ADR-0080](../../../docs/adr/0080-device-cards.md);
//! `docs/ux/second-device.md` section 3).
//!
//! ## What travels, and where
//!
//! A device card is `{updated_at_ms, last_active_ms, platform, model,
//! app_version, os, city, country}`: what a person needs to tell the phone in
//! their hand from a laptop at home on the Devices screen. It rides as a
//! DeviceGroup `ServiceEnvelope` (the ADR-0060 section 7 carrier), so it is
//! end-to-end encrypted to the account's own devices and nobody else. The
//! server sees a DeviceGroup row and nothing about the device (invariants
//! #1, #2, #5).
//!
//! ## What the envelope does NOT establish
//!
//! * **The card names nothing about its sender.** The sender is the
//!   MLS-authenticated DeviceGroup leaf. The receiver stores the card under
//!   that leaf's signature key resolved at receipt (C-68: never a leaf
//!   index, which OpenMLS reuses), and drops it when the leaf leaves.
//! * **Every field is the sender's statement.** A device can describe itself
//!   however it likes; the screen shows what it says. What protects the
//!   person is that only the account's own devices can send one.
//! * **It is meaningful only in the account's DeviceGroup.** A card met in a
//!   conversation group is dropped by the client.
//!
//! ## Wire shape
//!
//! ```text
//!   plaintext_payload =
//!       DEVICE_CARD_ENVELOPE_DOMAIN_V1 || version || updated_at_ms || last_active_ms
//!      └─────── 21 bytes, ASCII ──────┘ └─ 0x01 ─┘ └── 8 B, BE ───┘ └── 8 B, BE ────┘
//!     || platform || model_len || model || app_len || app_version || os_len || os
//!      └── 1 B ──┘ └── 1 B ───┘
//!     || city_len || city || country_len || country
//!
//!                     "lakko/device-card/v1\x00"
//! ```
//!
//! Every string is one-byte-length-prefixed.
//!
//! ## Bounds, and why a violation rejects the whole envelope
//!
//! * `platform` is one of the closed set [`DevicePlatform`]; any other byte
//!   is refused.
//! * `model` at most [`DEVICE_CARD_MODEL_MAX_BYTES`] bytes, `app_version` at
//!   most [`DEVICE_CARD_APP_VERSION_MAX_BYTES`], `os` (system name and
//!   version) at most [`DEVICE_CARD_OS_MAX_BYTES`], `city` at most
//!   [`DEVICE_CARD_CITY_MAX_BYTES`]. Each may be empty (not known). Each must
//!   be valid UTF-8, trimmed, with no control character, no line break and
//!   no Unicode format character (the profile's
//!   [`crate::profile::FORMAT_CHAR_RANGES`] table, with none of the display
//!   name's joiner exceptions: a device description needs none).
//! * `country` is empty or exactly two ASCII capital letters (an ISO 3166-1
//!   alpha-2 code; whether the code is assigned is not checked).
//!
//! An envelope violating any bound is **rejected as a whole**, and
//! [`compose_device_card`] refuses exactly what [`decode_device_card`]
//! refuses, since both run [`DeviceCard::validate`]. Text taken from an
//! operating system (a browser's user agent, a city database answer) goes
//! through [`DeviceCard::normalized`] first, which removes what the bounds
//! refuse instead of failing.
//!
//! ## Order
//!
//! [`device_card_is_newer`]: a receiver keeps the card with the greater
//! `updated_at_ms` per sending leaf key; equal is not newer, so the growth
//! re-send of a stored card (re-emitted verbatim, ADR-0058) is not an update.
//!
//! ## Cross-kind isolation
//!
//! The separator must be byte-distinct from, and not a prefix of or prefixed
//! by, every sibling envelope domain in the crate;
//! [`tests::separator_distinct_from_sibling_domains`] asserts it.

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::profile::is_format_char;

/// 21-byte domain separator prepended to the device-card payload.
///
/// The trailing `\x00` makes the prefix impossible for a UTF-8 free-text
/// payload to start with. **The `v1` names the envelope FAMILY**; the bytes
/// are frozen and the in-payload version byte is the knob that moves.
pub const DEVICE_CARD_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/device-card/v1\x00";

/// Schema-version byte carried first after the separator.
pub const DEVICE_CARD_VERSION_V1: u8 = 0x01;

/// Longest `model` in bytes.
pub const DEVICE_CARD_MODEL_MAX_BYTES: usize = 64;
/// Longest `app_version` in bytes.
pub const DEVICE_CARD_APP_VERSION_MAX_BYTES: usize = 32;
/// Longest `os` (system name and version) in bytes.
pub const DEVICE_CARD_OS_MAX_BYTES: usize = 64;
/// Longest `city` in bytes.
pub const DEVICE_CARD_CITY_MAX_BYTES: usize = 64;
/// Length of a non-empty `country`.
pub const DEVICE_CARD_COUNTRY_LEN: usize = 2;

/// Bytes after the separator every envelope carries: version, the two
/// stamps, platform, and the five length bytes.
const FIXED_PAYLOAD_LEN: usize = 1 + 8 + 8 + 1 + 5;

/// Shortest well-formed envelope: every string empty.
pub const DEVICE_CARD_ENVELOPE_MIN_LEN: usize =
    DEVICE_CARD_ENVELOPE_DOMAIN_V1.len() + FIXED_PAYLOAD_LEN;

/// Longest well-formed envelope: every string at its bound.
pub const DEVICE_CARD_ENVELOPE_MAX_LEN: usize = DEVICE_CARD_ENVELOPE_MIN_LEN
    + DEVICE_CARD_MODEL_MAX_BYTES
    + DEVICE_CARD_APP_VERSION_MAX_BYTES
    + DEVICE_CARD_OS_MAX_BYTES
    + DEVICE_CARD_CITY_MAX_BYTES
    + DEVICE_CARD_COUNTRY_LEN;

/// The closed set of platforms a card may name. The wire byte of each is
/// frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DevicePlatform {
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

impl DevicePlatform {
    /// The frozen wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Ios => 1,
            Self::Ipados => 2,
            Self::Macos => 3,
            Self::Android => 4,
            Self::Web => 5,
            Self::Desktop => 6,
        }
    }

    /// The platform a wire byte names, or `None` for any other byte.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Ios),
            2 => Some(Self::Ipados),
            3 => Some(Self::Macos),
            4 => Some(Self::Android),
            5 => Some(Self::Web),
            6 => Some(Self::Desktop),
            _ => None,
        }
    }

    /// Stable lower-case name (`ios`, `ipados`, `macos`, `android`, `web`,
    /// `desktop`), the spelling the ADR uses.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Ipados => "ipados",
            Self::Macos => "macos",
            Self::Android => "android",
            Self::Web => "web",
            Self::Desktop => "desktop",
        }
    }

    /// The platform a stable name names, or `None`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Ios,
            Self::Ipados,
            Self::Macos,
            Self::Android,
            Self::Web,
            Self::Desktop,
        ]
        .into_iter()
        .find(|p| p.name() == name)
    }
}

/// Which string field of a card a bound was violated in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceCardField {
    /// `model`.
    Model,
    /// `app_version`.
    AppVersion,
    /// `os`.
    Os,
    /// `city`.
    City,
    /// `country`.
    Country,
}

impl DeviceCardField {
    /// Stable name for reason codes.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::AppVersion => "appVersion",
            Self::Os => "os",
            Self::City => "city",
            Self::Country => "country",
        }
    }

    const fn max_bytes(self) -> usize {
        match self {
            Self::Model => DEVICE_CARD_MODEL_MAX_BYTES,
            Self::AppVersion => DEVICE_CARD_APP_VERSION_MAX_BYTES,
            Self::Os => DEVICE_CARD_OS_MAX_BYTES,
            Self::City => DEVICE_CARD_CITY_MAX_BYTES,
            Self::Country => DEVICE_CARD_COUNTRY_LEN,
        }
    }
}

/// What a device says about itself to the account's other devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCard {
    /// Sender-clock UNIX milliseconds at which this card was made. Orders
    /// cards from one leaf via [`device_card_is_newer`]. Re-emitted verbatim
    /// on a growth re-send.
    pub updated_at_ms: u64,
    /// Sender-clock UNIX milliseconds of the person's last action on the
    /// sending device.
    pub last_active_ms: u64,
    /// The device's platform.
    pub platform: DevicePlatform,
    /// The device's model, or for a browser "Chrome on macOS". May be empty.
    pub model: String,
    /// The app version, e.g. "1.4" or "web". May be empty.
    pub app_version: String,
    /// The system name and version, e.g. "iOS 27.0.1". May be empty.
    pub os: String,
    /// The city of the device's internet address, or empty.
    pub city: String,
    /// The country code of that city (two ASCII capital letters), or empty.
    pub country: String,
}

impl DeviceCard {
    /// Check every bound. The single rule both [`compose_device_card`] and
    /// [`decode_device_card`] run, so they cannot drift.
    ///
    /// # Errors
    ///
    /// [`DeviceCardError::Field`] naming the first field out of bounds.
    pub fn validate(&self) -> CoreResult<(), DeviceCardError> {
        for (field, text) in self.text_fields() {
            check_text(field, text)?;
        }
        check_country(&self.country)
    }

    /// The same card with every string made admissible: characters the
    /// bounds refuse removed (a line break becomes a space), whitespace
    /// trimmed, each string cut on a character boundary to its byte bound,
    /// and a country that is not two ASCII letters emptied (letters are
    /// upper-cased). The result always passes [`Self::validate`]. Used on
    /// text a device took from its operating system before composing.
    #[must_use]
    pub fn normalized(&self) -> Self {
        Self {
            updated_at_ms: self.updated_at_ms,
            last_active_ms: self.last_active_ms,
            platform: self.platform,
            model: normalize_text(&self.model, DeviceCardField::Model.max_bytes()),
            app_version: normalize_text(&self.app_version, DeviceCardField::AppVersion.max_bytes()),
            os: normalize_text(&self.os, DeviceCardField::Os.max_bytes()),
            city: normalize_text(&self.city, DeviceCardField::City.max_bytes()),
            country: normalize_country(&self.country),
        }
    }

    fn text_fields(&self) -> [(DeviceCardField, &str); 4] {
        [
            (DeviceCardField::Model, &self.model),
            (DeviceCardField::AppVersion, &self.app_version),
            (DeviceCardField::Os, &self.os),
            (DeviceCardField::City, &self.city),
        ]
    }
}

/// `true` when a card stamped `incoming_updated_at_ms` must replace one
/// stamped `stored_updated_at_ms` from the **same leaf key**. Strictly
/// greater wins; equal is not newer.
#[must_use]
pub const fn device_card_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose a device-card envelope: the inner plaintext handed to the
/// DeviceGroup `ServiceEnvelope` send path.
///
/// # Errors
///
/// Exactly the errors [`DeviceCard::validate`] returns.
pub fn compose_device_card(card: &DeviceCard) -> CoreResult<Vec<u8>, DeviceCardError> {
    card.validate()?;
    let mut out = Vec::with_capacity(
        DEVICE_CARD_ENVELOPE_MIN_LEN
            + card.model.len()
            + card.app_version.len()
            + card.os.len()
            + card.city.len()
            + card.country.len(),
    );
    out.extend_from_slice(DEVICE_CARD_ENVELOPE_DOMAIN_V1);
    out.push(DEVICE_CARD_VERSION_V1);
    out.extend_from_slice(&card.updated_at_ms.to_be_bytes());
    out.extend_from_slice(&card.last_active_ms.to_be_bytes());
    out.push(card.platform.to_byte());
    for (field, text) in card
        .text_fields()
        .into_iter()
        .chain([(DeviceCardField::Country, card.country.as_str())])
    {
        // `validate` bounded every length below `u8::MAX`; re-checked rather
        // than cast so a future bound change cannot truncate.
        let len = u8::try_from(text.len()).map_err(|_| DeviceCardError::Field {
            field,
            reason: DeviceCardFieldError::TooLong,
        })?;
        out.push(len);
        out.extend_from_slice(text.as_bytes());
    }
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(card))` — the separator matched and the card is within every
///   bound.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the card is malformed; the caller
///   drops it, never renders it.
///
/// # Errors
///
/// [`DeviceCardError::Truncated`], [`DeviceCardError::UnknownVersion`],
/// [`DeviceCardError::UnknownPlatform`], [`DeviceCardError::TrailingBytes`]
/// or [`DeviceCardError::Field`].
pub fn try_unwrap_device_card(plaintext: &[u8]) -> CoreResult<Option<DeviceCard>, DeviceCardError> {
    let Some(payload) = plaintext.strip_prefix(DEVICE_CARD_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let mut cur = payload;

    let version = take_u8(&mut cur)?;
    if version != DEVICE_CARD_VERSION_V1 {
        return Err(DeviceCardError::UnknownVersion(version));
    }
    let updated_at_ms = take_u64(&mut cur)?;
    let last_active_ms = take_u64(&mut cur)?;
    let platform_byte = take_u8(&mut cur)?;
    let platform = DevicePlatform::from_byte(platform_byte)
        .ok_or(DeviceCardError::UnknownPlatform(platform_byte))?;
    let model = take_string(&mut cur, DeviceCardField::Model)?;
    let app_version = take_string(&mut cur, DeviceCardField::AppVersion)?;
    let os = take_string(&mut cur, DeviceCardField::Os)?;
    let city = take_string(&mut cur, DeviceCardField::City)?;
    let country = take_string(&mut cur, DeviceCardField::Country)?;
    if !cur.is_empty() {
        return Err(DeviceCardError::TrailingBytes { count: cur.len() });
    }
    let card = DeviceCard {
        updated_at_ms,
        last_active_ms,
        platform,
        model,
        app_version,
        os,
        city,
        country,
    };
    card.validate()?;
    Ok(Some(card))
}

/// Strict standalone decoder: a non-card plaintext is a hard error.
///
/// # Errors
///
/// [`DeviceCardError::WrongDomain`], plus every error
/// [`try_unwrap_device_card`] returns.
pub fn decode_device_card(bytes: &[u8]) -> CoreResult<DeviceCard, DeviceCardError> {
    try_unwrap_device_card(bytes)?.ok_or(DeviceCardError::WrongDomain)
}

/// How a string field broke its bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeviceCardFieldError {
    /// Longer than the field's byte bound.
    #[error("too long")]
    TooLong,
    /// Not valid UTF-8.
    #[error("not UTF-8")]
    NotUtf8,
    /// Leading or trailing whitespace.
    #[error("not trimmed")]
    NotTrimmed,
    /// A control character, a line break or a format character.
    #[error("control or format character")]
    ControlOrFormatCharacter,
    /// A country that is neither empty nor two ASCII capital letters.
    #[error("not a two-letter country code")]
    NotACountryCode,
}

impl DeviceCardFieldError {
    const fn code(self) -> &'static str {
        match self {
            Self::TooLong => "tooLong",
            Self::NotUtf8 => "notUtf8",
            Self::NotTrimmed => "notTrimmed",
            Self::ControlOrFormatCharacter => "controlOrFormatCharacter",
            Self::NotACountryCode => "notACountryCode",
        }
    }
}

/// Why a device-card envelope was refused. A reason code: no variant
/// carries the offending text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeviceCardError {
    /// The plaintext does not carry the device-card separator. Only from
    /// [`decode_device_card`].
    #[error("plaintext does not carry the device-card domain separator")]
    WrongDomain,
    /// The payload ends before a declared field.
    #[error("device-card envelope is truncated")]
    Truncated,
    /// Bytes remain after the last field.
    #[error("device-card envelope has {count} trailing bytes")]
    TrailingBytes {
        /// How many bytes followed the last field.
        count: usize,
    },
    /// The version byte is not [`DEVICE_CARD_VERSION_V1`].
    #[error("device-card envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The platform byte is not in the closed set.
    #[error("device-card envelope names unknown platform {0:#04x}")]
    UnknownPlatform(u8),
    /// A string field is out of bounds.
    #[error("device-card {} field: {reason}", field.name())]
    Field {
        /// The field.
        field: DeviceCardField,
        /// How it broke its bound.
        reason: DeviceCardFieldError,
    },
}

impl DeviceCardError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::WrongDomain => "wrongDomain".to_string(),
            Self::Truncated => "truncated".to_string(),
            Self::TrailingBytes { .. } => "trailingBytes".to_string(),
            Self::UnknownVersion(_) => "unknownVersion".to_string(),
            Self::UnknownPlatform(_) => "unknownPlatform".to_string(),
            Self::Field { field, reason } => format!("{}.{}", field.name(), reason.code()),
        }
    }
}

/// A character no card string may carry.
fn is_refused_char(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') || is_format_char(c)
}

fn check_text(field: DeviceCardField, text: &str) -> CoreResult<(), DeviceCardError> {
    let err = |reason| DeviceCardError::Field { field, reason };
    if text.len() > field.max_bytes() {
        return Err(err(DeviceCardFieldError::TooLong));
    }
    if text.chars().any(is_refused_char) {
        return Err(err(DeviceCardFieldError::ControlOrFormatCharacter));
    }
    if text.trim() != text {
        return Err(err(DeviceCardFieldError::NotTrimmed));
    }
    Ok(())
}

fn check_country(country: &str) -> CoreResult<(), DeviceCardError> {
    if country.is_empty()
        || (country.len() == DEVICE_CARD_COUNTRY_LEN
            && country.bytes().all(|b| b.is_ascii_uppercase()))
    {
        Ok(())
    } else {
        Err(DeviceCardError::Field {
            field: DeviceCardField::Country,
            reason: DeviceCardFieldError::NotACountryCode,
        })
    }
}

fn normalize_text(input: &str, max_bytes: usize) -> String {
    let cleaned: String = input
        .chars()
        .filter_map(|c| {
            if matches!(c, '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}' | '\u{0085}') {
                Some(' ')
            } else if is_refused_char(c) {
                None
            } else {
                Some(c)
            }
        })
        .collect();
    let mut out = String::with_capacity(max_bytes.min(cleaned.len()));
    for c in cleaned.trim().chars() {
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out.trim_end().to_string()
}

fn normalize_country(input: &str) -> String {
    let code = input.trim().to_ascii_uppercase();
    if code.len() == DEVICE_CARD_COUNTRY_LEN && code.bytes().all(|b| b.is_ascii_uppercase()) {
        code
    } else {
        String::new()
    }
}

fn take_u8(cur: &mut &[u8]) -> CoreResult<u8, DeviceCardError> {
    let (&first, rest) = cur.split_first().ok_or(DeviceCardError::Truncated)?;
    *cur = rest;
    Ok(first)
}

fn take_u64(cur: &mut &[u8]) -> CoreResult<u64, DeviceCardError> {
    let bytes: [u8; 8] = take(cur, 8)?
        .try_into()
        .map_err(|_| DeviceCardError::Truncated)?;
    Ok(u64::from_be_bytes(bytes))
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> CoreResult<&'a [u8], DeviceCardError> {
    if cur.len() < n {
        return Err(DeviceCardError::Truncated);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

fn take_string(cur: &mut &[u8], field: DeviceCardField) -> CoreResult<String, DeviceCardError> {
    let len = usize::from(take_u8(cur)?);
    if len > field.max_bytes() {
        return Err(DeviceCardError::Field {
            field,
            reason: if field == DeviceCardField::Country {
                DeviceCardFieldError::NotACountryCode
            } else {
                DeviceCardFieldError::TooLong
            },
        });
    }
    let bytes = take(cur, len)?;
    core::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|_| DeviceCardError::Field {
            field,
            reason: DeviceCardFieldError::NotUtf8,
        })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> DeviceCard {
        DeviceCard {
            updated_at_ms: 1_700_000_000_000,
            last_active_ms: 1_699_999_000_000,
            platform: DevicePlatform::Ios,
            model: "iPhone 16 Pro".to_string(),
            app_version: "1.4".to_string(),
            os: "iOS 27.0.1".to_string(),
            city: "Almere".to_string(),
            country: "NL".to_string(),
        }
    }

    /// Hand-build an envelope bypassing `compose`'s validation.
    fn raw(version: u8, platform: u8, strings: [&[u8]; 5]) -> Vec<u8> {
        let mut out = DEVICE_CARD_ENVELOPE_DOMAIN_V1.to_vec();
        out.push(version);
        out.extend_from_slice(&7u64.to_be_bytes());
        out.extend_from_slice(&5u64.to_be_bytes());
        out.push(platform);
        for s in strings {
            out.push(u8::try_from(s.len()).unwrap());
            out.extend_from_slice(s);
        }
        out
    }

    #[test]
    fn compose_then_decode_round_trips() {
        let mut empty = card();
        empty.model = String::new();
        empty.app_version = String::new();
        empty.os = String::new();
        empty.city = String::new();
        empty.country = String::new();
        let mut web = card();
        web.platform = DevicePlatform::Web;
        web.model = "Chrome on macOS".to_string();
        web.city = "Санкт-Петербург".to_string();
        web.country = "RU".to_string();
        for c in [card(), empty, web] {
            let wire = compose_device_card(&c).unwrap();
            assert_eq!(decode_device_card(&wire).unwrap(), c);
        }
    }

    #[test]
    fn every_platform_round_trips_and_no_other_byte_is_a_platform() {
        for b in 0..=u8::MAX {
            if let Some(p) = DevicePlatform::from_byte(b) {
                assert_eq!(p.to_byte(), b);
                assert_eq!(DevicePlatform::from_name(p.name()), Some(p));
                let mut c = card();
                c.platform = p;
                let wire = compose_device_card(&c).unwrap();
                assert_eq!(decode_device_card(&wire).unwrap().platform, p);
            } else {
                assert!(!(1..=6).contains(&b));
                let wire = raw(1, b, [b"", b"", b"", b"", b""]);
                assert_eq!(
                    decode_device_card(&wire),
                    Err(DeviceCardError::UnknownPlatform(b))
                );
            }
        }
        assert_eq!(DevicePlatform::from_name("windows"), None);
    }

    #[test]
    fn compose_layout_is_pinned() {
        let wire = compose_device_card(&card()).unwrap();
        let sep = DEVICE_CARD_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(sep, 21);
        assert_eq!(&wire[..sep], b"lakko/device-card/v1\x00");
        assert_eq!(wire[sep], DEVICE_CARD_VERSION_V1);
        assert_eq!(&wire[sep + 1..sep + 9], &1_700_000_000_000u64.to_be_bytes());
        assert_eq!(
            &wire[sep + 9..sep + 17],
            &1_699_999_000_000u64.to_be_bytes()
        );
        assert_eq!(wire[sep + 17], 1);
        assert_eq!(wire[sep + 18], 13);
        assert_eq!(&wire[sep + 19..sep + 32], b"iPhone 16 Pro");
        assert_eq!(&wire[wire.len() - 3..], b"\x02NL");
    }

    #[test]
    fn length_bounds_are_the_documented_construction() {
        assert_eq!(DEVICE_CARD_ENVELOPE_MIN_LEN, 21 + 23);
        assert_eq!(DEVICE_CARD_ENVELOPE_MAX_LEN, 44 + 64 + 32 + 64 + 64 + 2);
        let max = DeviceCard {
            updated_at_ms: u64::MAX,
            last_active_ms: u64::MAX,
            platform: DevicePlatform::Desktop,
            model: "m".repeat(64),
            app_version: "a".repeat(32),
            os: "o".repeat(64),
            city: "c".repeat(64),
            country: "DE".to_string(),
        };
        assert_eq!(
            compose_device_card(&max).unwrap().len(),
            DEVICE_CARD_ENVELOPE_MAX_LEN
        );
    }

    #[test]
    fn non_card_plaintext_falls_through() {
        assert_eq!(try_unwrap_device_card(b"hello"), Ok(None));
        assert_eq!(try_unwrap_device_card(&[]), Ok(None));
        assert_eq!(
            decode_device_card(b"hello"),
            Err(DeviceCardError::WrongDomain)
        );
        assert_eq!(
            try_unwrap_device_card(DEVICE_CARD_ENVELOPE_DOMAIN_V1),
            Err(DeviceCardError::Truncated)
        );
    }

    #[test]
    fn every_truncation_point_is_refused() {
        let wire = compose_device_card(&card()).unwrap();
        for cut in DEVICE_CARD_ENVELOPE_DOMAIN_V1.len()..wire.len() {
            assert_eq!(
                try_unwrap_device_card(&wire[..cut]),
                Err(DeviceCardError::Truncated),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn trailing_bytes_and_unknown_versions_are_refused() {
        let mut wire = compose_device_card(&card()).unwrap();
        wire.push(0);
        assert_eq!(
            decode_device_card(&wire),
            Err(DeviceCardError::TrailingBytes { count: 1 })
        );
        for v in [0, 2, 0xFF] {
            assert_eq!(
                decode_device_card(&raw(v, 1, [b"", b"", b"", b"", b""])),
                Err(DeviceCardError::UnknownVersion(v))
            );
        }
    }

    #[test]
    fn every_bound_is_refused_on_both_compose_and_decode() {
        type Set = fn(&mut DeviceCard, String);
        let fields: [(DeviceCardField, usize, Set); 4] = [
            (DeviceCardField::Model, 0, |c, s| c.model = s),
            (DeviceCardField::AppVersion, 1, |c, s| c.app_version = s),
            (DeviceCardField::Os, 2, |c, s| c.os = s),
            (DeviceCardField::City, 3, |c, s| c.city = s),
        ];
        for (field, slot, set) in fields {
            let max = field.max_bytes();
            let cases: [(String, DeviceCardFieldError); 6] = [
                ("x".repeat(max + 1), DeviceCardFieldError::TooLong),
                (" lead".to_string(), DeviceCardFieldError::NotTrimmed),
                (
                    "a\nb".to_string(),
                    DeviceCardFieldError::ControlOrFormatCharacter,
                ),
                (
                    "a\u{2028}b".to_string(),
                    DeviceCardFieldError::ControlOrFormatCharacter,
                ),
                (
                    "a\u{202E}b".to_string(),
                    DeviceCardFieldError::ControlOrFormatCharacter,
                ),
                (
                    "a\u{200D}b".to_string(),
                    DeviceCardFieldError::ControlOrFormatCharacter,
                ),
            ];
            for (text, reason) in cases {
                let mut c = card();
                set(&mut c, text.clone());
                let expected = DeviceCardError::Field { field, reason };
                assert_eq!(compose_device_card(&c), Err(expected), "{text:?}");
                let mut strings: [&[u8]; 5] = [b"", b"", b"", b"", b""];
                strings[slot] = text.as_bytes();
                assert_eq!(decode_device_card(&raw(1, 1, strings)), Err(expected));
            }
            let mut strings: [&[u8]; 5] = [b"", b"", b"", b"", b""];
            strings[slot] = &[0xFF, 0xFE];
            assert_eq!(
                decode_device_card(&raw(1, 1, strings)),
                Err(DeviceCardError::Field {
                    field,
                    reason: DeviceCardFieldError::NotUtf8
                })
            );
        }
        for bad in ["nl", "N", "NLD", "1A", "\u{00C4}X"] {
            let mut c = card();
            c.country = bad.to_string();
            let expected = DeviceCardError::Field {
                field: DeviceCardField::Country,
                reason: DeviceCardFieldError::NotACountryCode,
            };
            assert_eq!(compose_device_card(&c), Err(expected), "{bad:?}");
            assert_eq!(
                decode_device_card(&raw(1, 1, [b"", b"", b"", b"", bad.as_bytes()])),
                Err(expected),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn normalized_always_validates_and_keeps_good_text() {
        assert_eq!(card().normalized(), card());
        let messy = DeviceCard {
            model: format!("  Pixel\n7a\u{200B}{}  ", "x".repeat(100)),
            app_version: "\t1.4\r".to_string(),
            os: "Android\u{0}15".to_string(),
            city: "\u{1F600}".repeat(20),
            country: " nl ".to_string(),
            ..card()
        };
        let n = messy.normalized();
        assert!(n.validate().is_ok());
        assert!(n.model.starts_with("Pixel 7a"));
        assert!(n.model.len() <= DEVICE_CARD_MODEL_MAX_BYTES);
        assert_eq!(n.app_version, "1.4");
        assert_eq!(n.os, "Android15");
        assert_eq!(n.city, "\u{1F600}".repeat(16));
        assert_eq!(n.country, "NL");
        let mut odd = card();
        odd.country = "Netherlands".to_string();
        assert_eq!(odd.normalized().country, "");
    }

    #[test]
    fn newer_is_strictly_greater() {
        assert!(device_card_is_newer(2, 1));
        assert!(!device_card_is_newer(1, 1));
        assert!(!device_card_is_newer(0, 1));
    }

    #[test]
    fn a_growth_resend_of_the_stored_card_is_not_an_update() {
        let wire = compose_device_card(&card()).unwrap();
        let resent = decode_device_card(&wire).unwrap();
        assert!(!device_card_is_newer(
            resent.updated_at_ms,
            card().updated_at_ms
        ));
    }

    #[test]
    fn error_messages_never_echo_the_text() {
        let err = decode_device_card(&raw(1, 1, [b"Secret\n", b"", b"", b"", b""])).unwrap_err();
        assert!(!err.to_string().contains("Secret"));
        assert!(!format!("{err:?}").contains("Secret"));
    }

    #[test]
    fn error_codes_are_frozen() {
        assert_eq!(DeviceCardError::WrongDomain.code(), "wrongDomain");
        assert_eq!(DeviceCardError::Truncated.code(), "truncated");
        assert_eq!(
            DeviceCardError::TrailingBytes { count: 1 }.code(),
            "trailingBytes"
        );
        assert_eq!(DeviceCardError::UnknownVersion(2).code(), "unknownVersion");
        assert_eq!(
            DeviceCardError::UnknownPlatform(9).code(),
            "unknownPlatform"
        );
        assert_eq!(
            DeviceCardError::Field {
                field: DeviceCardField::AppVersion,
                reason: DeviceCardFieldError::TooLong
            }
            .code(),
            "appVersion.tooLong"
        );
        assert_eq!(
            DeviceCardError::Field {
                field: DeviceCardField::Country,
                reason: DeviceCardFieldError::NotACountryCode
            }
            .code(),
            "country.notACountryCode"
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_GID_DERIVE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::group_photo::GROUP_PHOTO_ENVELOPE_DOMAIN_V1,
            crate::shared_channels::SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::key_rotation::KEY_ROTATION_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for d in other_domains {
            assert_ne!(*d, DEVICE_CARD_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(DEVICE_CARD_ENVELOPE_DOMAIN_V1)
                    && !DEVICE_CARD_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }

    #[test]
    fn a_card_dispatches_as_a_card_and_never_as_a_sibling() {
        use crate::incoming_envelope::{dispatch_incoming_envelope, IncomingEnvelope};
        use crate::profile::try_unwrap_profile;
        use crate::visibility::try_unwrap_visibility;

        let wire = compose_device_card(&card()).unwrap();
        assert_eq!(
            dispatch_incoming_envelope(&wire),
            Ok(IncomingEnvelope::DeviceCard(card()))
        );
        assert!(try_unwrap_profile(&wire).unwrap().is_none());
        assert!(try_unwrap_visibility(&wire).unwrap().is_none());
        assert!(crate::group_name::try_unwrap_group_name(&wire)
            .unwrap()
            .is_none());
        for sibling in [
            crate::media_state::compose_media_state(true, true),
            crate::committer_beacon::compose_committer_beacon(),
            crate::push_routing::compose_push_routing_secret(7, &[0x11; 32]),
        ] {
            assert_eq!(try_unwrap_device_card(&sibling), Ok(None));
        }
        // A malformed card is an error, never an intro message.
        let mut bad = wire;
        bad.push(1);
        assert!(dispatch_incoming_envelope(&bad).is_err());
    }
}
