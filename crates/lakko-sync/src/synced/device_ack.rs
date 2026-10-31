// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The device-acknowledgement register and its envelope
//! (`docs/ux/identity-recovery.md` section 17.5, amended 2026-10-29).
//!
//! When a device joins the account, every other device of the account warns
//! the person: a system notification, a badge on Settings, the new device's
//! row highlighted in Settings > Devices. Once the person has seen it on ANY
//! device, the warning goes away on EVERY device. This register is what
//! carries "seen": one register per device signature key, written once by the
//! device where the person saw it, folded by the ordinary rule
//! ([`super::state`]).
//!
//! ## The register
//!
//! `device key -> acknowledged at`. There is no "unacknowledge": a device
//! that leaves and comes back with the same leaf key (an ADR-0081 rejoin) is
//! the same device the person already saw. It is **write-once at the
//! writer** ([`super::SyncedState::acknowledge_device`] refuses to re-stamp a
//! held register), so a device never mints a second stamp for a device that
//! was already acknowledged; two devices acknowledging concurrently converge
//! on one value by the ordinary rule. The value's time orders nothing and
//! decides nothing; it is shown nowhere and kept for diagnostics.
//!
//! ## Wire
//!
//! ```text
//! domain(25) "lakko/sync/device-ack/v1\0"
//! version(1) = 0x01
//! count(1)                    1..=16
//! count x {
//!     hlc(8, BE)              the register's ORIGINAL stamp
//!     device_key(32)          the acknowledged device's signature key
//!     acknowledged_at_ms(8, BE)
//! }
//! ```
//!
//! Entries are sorted by device key, strictly increasing (no duplicates);
//! nothing may trail. The encoding is canonical. Every violation refuses the
//! whole envelope.
//!
//! ## Trust
//!
//! The bytes arrive from another device of the same account, inside an MLS
//! application message of the DeviceGroup, or from a backup. They are
//! authenticated as "one of my devices" and nothing more. An acknowledgement
//! only removes a warning; a device of the account could always remove the
//! warning by other means (it can remove the new device, or dismiss), so a
//! hostile device of the account gains nothing it did not already hold. The
//! decoder is bounded and total on arbitrary input.

use thiserror::Error;

use super::entry::{take_n, take_u64, take_u8, Stamped};

/// Length of a device signature key.
pub const DEVICE_ACK_KEY_LEN: usize = 32;

/// Domain separator of the device-acknowledgement envelope, trailing NUL
/// included. Frozen.
pub const DEVICE_ACK_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/device-ack/v1\0";

/// The version byte this build writes and accepts.
pub const DEVICE_ACK_ENVELOPE_VERSION_V1: u8 = 0x01;

/// Most entries one envelope carries.
pub const DEVICE_ACK_ENVELOPE_MAX_ENTRIES: usize = 16;

const ENTRY_LEN: usize = 8 + DEVICE_ACK_KEY_LEN + 8;

/// Shortest valid envelope: the header and one entry.
pub const DEVICE_ACK_ENVELOPE_MIN_LEN: usize =
    DEVICE_ACK_ENVELOPE_DOMAIN_V1.len() + 1 + 1 + ENTRY_LEN;

/// Longest valid envelope.
pub const DEVICE_ACK_ENVELOPE_MAX_LEN: usize =
    DEVICE_ACK_ENVELOPE_DOMAIN_V1.len() + 1 + 1 + DEVICE_ACK_ENVELOPE_MAX_ENTRIES * ENTRY_LEN;

/// One device-acknowledgement register value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAckEntry {
    /// The acknowledged device's signature public key.
    pub device_key: [u8; DEVICE_ACK_KEY_LEN],
    /// Wall-clock millis of the acknowledgement. Decides nothing.
    pub acknowledged_at_ms: u64,
}

impl DeviceAckEntry {
    /// Append the body (key then time) to `out`.
    pub(crate) fn encode_body(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.device_key);
        out.extend_from_slice(&self.acknowledged_at_ms.to_be_bytes());
    }

    /// Read a body written by [`Self::encode_body`].
    pub(crate) fn take_body(cur: &mut &[u8]) -> Result<Self, super::entry::CodecError> {
        let key = take_n(cur, DEVICE_ACK_KEY_LEN)?;
        let mut device_key = [0u8; DEVICE_ACK_KEY_LEN];
        device_key.copy_from_slice(key);
        let acknowledged_at_ms = take_u64(cur)?;
        Ok(Self {
            device_key,
            acknowledged_at_ms,
        })
    }
}

/// Why a device-acknowledgement envelope was refused. A reason, never the
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeviceAckEnvelopeError {
    /// The bytes do not open with [`DEVICE_ACK_ENVELOPE_DOMAIN_V1`].
    #[error("not a device-acknowledgement envelope")]
    WrongDomain,
    /// The version byte is not [`DEVICE_ACK_ENVELOPE_VERSION_V1`].
    #[error("unknown device-acknowledgement envelope version")]
    UnknownVersion,
    /// The count is zero or above [`DEVICE_ACK_ENVELOPE_MAX_ENTRIES`].
    #[error("device-acknowledgement envelope count is out of bounds")]
    Count,
    /// The bytes ended early.
    #[error("device-acknowledgement envelope is truncated")]
    Truncated,
    /// Entries were not strictly increasing by device key.
    #[error("device-acknowledgement envelope is not canonical")]
    NotCanonical,
    /// Bytes remained after the last entry.
    #[error("trailing bytes in device-acknowledgement envelope")]
    Trailing,
}

impl DeviceAckEnvelopeError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion => "unknownVersion",
            Self::Count => "count",
            Self::Truncated => "truncated",
            Self::NotCanonical => "notCanonical",
            Self::Trailing => "trailing",
        }
    }
}

/// `true` when `bytes` open with the device-acknowledgement envelope domain:
/// the cheap routing question a client asks of a DeviceGroup service
/// envelope. Says nothing about validity.
#[must_use]
pub fn is_device_ack_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(DEVICE_ACK_ENVELOPE_DOMAIN_V1)
}

/// Compose one envelope of 1..=16 stamped registers, sorted by device key.
///
/// # Errors
///
/// [`DeviceAckEnvelopeError::Count`] for none or too many entries,
/// [`DeviceAckEnvelopeError::NotCanonical`] for two entries of one device.
pub fn compose_device_ack_envelope(
    entries: &[Stamped<DeviceAckEntry>],
) -> Result<Vec<u8>, DeviceAckEnvelopeError> {
    if entries.is_empty() || entries.len() > DEVICE_ACK_ENVELOPE_MAX_ENTRIES {
        return Err(DeviceAckEnvelopeError::Count);
    }
    let count = u8::try_from(entries.len()).map_err(|_| DeviceAckEnvelopeError::Count)?;
    let mut sorted: Vec<&Stamped<DeviceAckEntry>> = entries.iter().collect();
    sorted.sort_by_key(|e| e.value.device_key);
    let mut out =
        Vec::with_capacity(DEVICE_ACK_ENVELOPE_DOMAIN_V1.len() + 2 + entries.len() * ENTRY_LEN);
    out.extend_from_slice(DEVICE_ACK_ENVELOPE_DOMAIN_V1);
    out.push(DEVICE_ACK_ENVELOPE_VERSION_V1);
    out.push(count);
    let mut previous: Option<&[u8; DEVICE_ACK_KEY_LEN]> = None;
    for e in sorted {
        if previous == Some(&e.value.device_key) {
            return Err(DeviceAckEnvelopeError::NotCanonical);
        }
        previous = Some(&e.value.device_key);
        out.extend_from_slice(&e.hlc.to_be_bytes());
        e.value.encode_body(&mut out);
    }
    Ok(out)
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`DeviceAckEnvelopeError`]; the domain is checked first, then the
/// version, then the count, then each entry, then trailing bytes.
pub fn decode_device_ack_envelope(
    bytes: &[u8],
) -> Result<Vec<Stamped<DeviceAckEntry>>, DeviceAckEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(DEVICE_ACK_ENVELOPE_DOMAIN_V1)
        .ok_or(DeviceAckEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| DeviceAckEnvelopeError::Truncated)?;
    if version != DEVICE_ACK_ENVELOPE_VERSION_V1 {
        return Err(DeviceAckEnvelopeError::UnknownVersion);
    }
    let count = usize::from(take_u8(&mut cur).map_err(|_| DeviceAckEnvelopeError::Truncated)?);
    if count == 0 || count > DEVICE_ACK_ENVELOPE_MAX_ENTRIES {
        return Err(DeviceAckEnvelopeError::Count);
    }
    let mut out: Vec<Stamped<DeviceAckEntry>> = Vec::with_capacity(count);
    for _ in 0..count {
        let hlc = take_u64(&mut cur).map_err(|_| DeviceAckEnvelopeError::Truncated)?;
        let value =
            DeviceAckEntry::take_body(&mut cur).map_err(|_| DeviceAckEnvelopeError::Truncated)?;
        if let Some(last) = out.last() {
            if last.value.device_key >= value.device_key {
                return Err(DeviceAckEnvelopeError::NotCanonical);
            }
        }
        out.push(Stamped { hlc, value });
    }
    if !cur.is_empty() {
        return Err(DeviceAckEnvelopeError::Trailing);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ack(key: u8, hlc: u64) -> Stamped<DeviceAckEntry> {
        Stamped {
            hlc,
            value: DeviceAckEntry {
                device_key: [key; DEVICE_ACK_KEY_LEN],
                acknowledged_at_ms: 1_700_000_000_000 + u64::from(key),
            },
        }
    }

    #[test]
    fn the_layout_is_pinned() {
        let wire = compose_device_ack_envelope(&[ack(1, 7)]).unwrap_or_default();
        assert_eq!(wire.len(), DEVICE_ACK_ENVELOPE_MIN_LEN);
        assert_eq!(DEVICE_ACK_ENVELOPE_DOMAIN_V1.len(), 25);
        assert_eq!(DEVICE_ACK_ENVELOPE_MIN_LEN, 75);
        assert_eq!(DEVICE_ACK_ENVELOPE_MAX_LEN, 25 + 2 + 16 * 48);
        assert!(wire.starts_with(DEVICE_ACK_ENVELOPE_DOMAIN_V1));
        assert_eq!(wire[25], 0x01);
        assert_eq!(wire[26], 1);
        assert_eq!(&wire[27..35], &7u64.to_be_bytes());
        assert_eq!(&wire[35..67], &[1u8; 32]);
        assert!(is_device_ack_envelope(&wire));
        assert!(!is_device_ack_envelope(b"lakko/sync/username/v1\0"));
    }

    #[test]
    fn compose_sorts_and_refuses_duplicates_and_bad_counts() {
        let wire = compose_device_ack_envelope(&[ack(9, 1), ack(3, 2)]).unwrap_or_default();
        let back = decode_device_ack_envelope(&wire).unwrap_or_default();
        assert_eq!(back, vec![ack(3, 2), ack(9, 1)]);
        assert_eq!(
            compose_device_ack_envelope(&[ack(3, 1), ack(3, 2)]),
            Err(DeviceAckEnvelopeError::NotCanonical)
        );
        assert_eq!(
            compose_device_ack_envelope(&[]),
            Err(DeviceAckEnvelopeError::Count)
        );
        let many: Vec<_> = (0..17).map(|k| ack(k, 1)).collect();
        assert_eq!(
            compose_device_ack_envelope(&many),
            Err(DeviceAckEnvelopeError::Count)
        );
    }

    #[test]
    fn decode_refuses_every_malformed_shape() {
        let wire = compose_device_ack_envelope(&[ack(1, 1), ack(2, 2)]).unwrap_or_default();
        let mut v = wire.clone();
        v[25] = 2;
        assert_eq!(
            decode_device_ack_envelope(&v),
            Err(DeviceAckEnvelopeError::UnknownVersion)
        );
        let mut c = wire.clone();
        c[26] = 0;
        assert_eq!(
            decode_device_ack_envelope(&c),
            Err(DeviceAckEnvelopeError::Count)
        );
        let mut t = wire.clone();
        t.push(0);
        assert_eq!(
            decode_device_ack_envelope(&t),
            Err(DeviceAckEnvelopeError::Trailing)
        );
        for cut in 0..wire.len() {
            assert!(decode_device_ack_envelope(&wire[..cut]).is_err(), "{cut}");
        }
        // Two entries out of order.
        let mut swapped = wire[..27].to_vec();
        swapped.extend_from_slice(&wire[27 + ENTRY_LEN..]);
        swapped.extend_from_slice(&wire[27..27 + ENTRY_LEN]);
        assert_eq!(
            decode_device_ack_envelope(&swapped),
            Err(DeviceAckEnvelopeError::NotCanonical)
        );
        assert_eq!(
            decode_device_ack_envelope(b"lakko/sync/username/v1\0\x01"),
            Err(DeviceAckEnvelopeError::WrongDomain)
        );
    }

    proptest! {
        #[test]
        fn decode_is_total_and_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..900)) {
            let mut input = DEVICE_ACK_ENVELOPE_DOMAIN_V1.to_vec();
            input.extend_from_slice(&bytes);
            if let Ok(entries) = decode_device_ack_envelope(&input) {
                prop_assert_eq!(compose_device_ack_envelope(&entries), Ok(input));
            }
        }

        #[test]
        fn round_trip(keys in proptest::collection::btree_set(any::<u8>(), 1..=16), hlc in any::<u64>()) {
            let entries: Vec<_> = keys.iter().map(|k| ack(*k, hlc)).collect();
            let wire = compose_device_ack_envelope(&entries).unwrap_or_default();
            prop_assert!(wire.len() <= DEVICE_ACK_ENVELOPE_MAX_LEN);
            prop_assert_eq!(decode_device_ack_envelope(&wire), Ok(entries));
        }
    }
}
