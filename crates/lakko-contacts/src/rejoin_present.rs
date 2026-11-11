// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Rejoin presence note — "a device of this account is running" (ADR-0081,
//! third amendment, 2026-11-11).
//!
//! A device restored with no usable DeviceGroup state knocks on the
//! account's DeviceGroup and waits for an answer. Every running device of
//! the account holds a long poll on those knocks, and on one that names the
//! account's login handle it posts this note to the knock's rendezvous at
//! once, before any directory lookup, so the answer costs one round trip.
//! The new device reads the note as "a device is online; wait for its
//! Welcome". No note and no Welcome within the answer window means every
//! listed device is offline, and the new device founds the DeviceGroup
//! itself.
//!
//! ## Wire shape
//!
//! ```text
//!   rendezvous_row = REJOIN_PRESENT_DOMAIN_V1 || version_byte
//!                    └──── 24 bytes, ASCII ───┘ └─ 0x01 ─┘
//!                    "lakko/rejoin-present/v1\x00"
//! ```
//!
//! A fixed 25 bytes and no fields: what the note says is that it arrived.
//! It is not authenticated (a rendezvous row is written by name, ADR-0053),
//! so a malicious Delivery Service can forge one or withhold every answer;
//! the first makes the new device wait for a Welcome that never comes, the
//! second makes it found, and the DS can cause both without the note. The
//! note never carries or unlocks key material. A Welcome on the same
//! rendezvous is told apart by this exact match: a Welcome is an MLS message
//! and never starts with the domain.

/// 24-byte domain separator. Frozen.
pub const REJOIN_PRESENT_DOMAIN_V1: &[u8] = b"lakko/rejoin-present/v1\x00";

/// Schema version, the single byte after the domain.
pub const REJOIN_PRESENT_VERSION_V1: u8 = 0x01;

/// Whole-note length.
pub const REJOIN_PRESENT_LEN: usize = REJOIN_PRESENT_DOMAIN_V1.len() + 1;

/// Compose the note. Infallible and byte-identical on every call.
#[must_use]
pub fn compose_rejoin_present() -> Vec<u8> {
    let mut out = Vec::with_capacity(REJOIN_PRESENT_LEN);
    out.extend_from_slice(REJOIN_PRESENT_DOMAIN_V1);
    out.push(REJOIN_PRESENT_VERSION_V1);
    out
}

/// Is `row` exactly a presence note? Anything else (a Welcome, another
/// version, a truncated or padded note) is `false`.
#[must_use]
pub fn is_rejoin_present(row: &[u8]) -> bool {
    row.len() == REJOIN_PRESENT_LEN
        && row.starts_with(REJOIN_PRESENT_DOMAIN_V1)
        && row[REJOIN_PRESENT_DOMAIN_V1.len()] == REJOIN_PRESENT_VERSION_V1
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_composed_note_is_recognised_and_has_the_advertised_length() {
        let note = compose_rejoin_present();
        assert_eq!(note.len(), REJOIN_PRESENT_LEN);
        assert_eq!(REJOIN_PRESENT_LEN, 25);
        assert!(is_rejoin_present(&note));
    }

    #[test]
    fn another_version_a_truncation_or_padding_is_not_a_note() {
        let mut note = compose_rejoin_present();
        assert!(!is_rejoin_present(&note[..note.len() - 1]));
        let mut padded = note.clone();
        padded.push(0);
        assert!(!is_rejoin_present(&padded));
        let last = note.len() - 1;
        note[last] = 0x02;
        assert!(!is_rejoin_present(&note));
        assert!(!is_rejoin_present(&[]));
    }

    proptest! {
        #[test]
        fn only_the_composed_bytes_are_a_note(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            prop_assert_eq!(is_rejoin_present(&bytes), bytes == compose_rejoin_present());
        }
    }
}
