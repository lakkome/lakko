// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim CLASSIFY surface for the ADR-0098 account-deleted envelope.
//!
//! A deleting device posts it as an ordinary MLS application message in every
//! conversation, so it can be the newest row when a push wake arrives. The
//! NSE must stay silent about it: `parse_body_envelope` already classifies a
//! `lakko/` plaintext as a never-rendered `ServiceEnvelope`; this is the
//! explicit, length-pinned recognition the NSE's pre-filter keys on.
//!
//! Recognise and the length only (C-59): no verify, no compose. Marking a
//! person deleted is the app's decision, taken under the C-68 sender check;
//! the NSE stores nothing.

use lakko_contacts::account_deleted::{decode_account_deleted, ACCOUNT_DELETED_ENVELOPE_LEN};

/// Whether `plaintext` has the account-deleted layout. Fail-closed: a
/// malformed envelope with the right separator answers `false`.
#[uniffi::export]
#[must_use]
pub fn is_account_deleted_envelope(plaintext: Vec<u8>) -> bool {
    decode_account_deleted(&plaintext).is_ok()
}

/// The one length of an account-deleted envelope.
#[uniffi::export]
#[must_use]
pub fn account_deleted_envelope_len() -> u32 {
    u32::try_from(ACCOUNT_DELETED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::account_deleted::ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1;

    fn wire() -> Vec<u8> {
        let mut w = ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1.to_vec();
        w.push(0x01);
        w.extend_from_slice(&[0x5a; 64]);
        w
    }

    #[test]
    fn recognises_the_layout_and_refuses_chat_and_damage() {
        assert!(is_account_deleted_envelope(wire()));
        assert!(!is_account_deleted_envelope(b"hello".to_vec()));
        let mut short = wire();
        short.pop();
        assert!(!is_account_deleted_envelope(short));
    }

    /// The Swift pre-filter may hard-code the length and the first byte; a
    /// wire revision must fail here, not in a notification.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(account_deleted_envelope_len(), 90);
        assert_eq!(
            ACCOUNT_DELETED_ENVELOPE_DOMAIN_V1,
            b"lakko/account-deleted/v1\x00"
        );
        assert_eq!(wire()[0], 0x6c);
    }
}
