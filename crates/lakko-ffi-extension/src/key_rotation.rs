// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` key-rotation surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0074 key-rotation envelope).
//!
//! ## Why the NSE needs this at all
//!
//! A key-rotation envelope is an MLS application message in the account's
//! own DeviceGroup. The NSE's background decrypt is driven by a group wake,
//! and [`crate::attachments::parse_body_envelope`]'s deliberate lossy-UTF-8
//! fallback would degrade any service envelope it does not recognise to a
//! `Chat` body: unfiltered, a notification would read
//! `lakko/key-rotation/v1` followed by bytes of the stamp. This arm is here
//! so that it never does, should a DeviceGroup row ever reach this path.
//!
//! ## Scope: a YES / NO answer and the length, NOTHING else
//!
//! [`is_key_rotation_envelope`] answers "is this a key-rotation envelope I
//! must not render?" and returns **no field**. That is deliberate and
//! stricter than the sibling arms, which return what they decode: this
//! envelope carries the entropy of the account's new recovery phrase. The
//! notification extension has no use for it (it adopts nothing, signs
//! nothing and stores nothing: C-59), so the bytes are decoded, judged and
//! dropped inside this call and never cross into Swift. **No compose, no
//! verify, no adopt lives here.** Adopting a new account seed is the
//! foreground app's decision, on its own receive path.
//!
//! [`key_rotation_envelope_len`] is what the NSE's cheap pre-filter needs,
//! exported rather than hand-copied into Swift (the ADR-0044 drift class: a
//! stale literal fails in the DANGEROUS direction, silently ceasing to
//! suppress).
//!
//! ## A malformed envelope is NOT classified here
//!
//! The decode is strict: a plaintext carrying the key-rotation domain but
//! failing its shape answers `false`, exactly as any other plaintext does,
//! and the NSE's filter chain falls through. The app's own dispatcher drops
//! such an envelope instead. This mirrors every sibling arm; the NSE's
//! last-resort guard for anything opening with `lakko/` (the
//! `ServiceEnvelope` body kind of [`crate::attachments`]) is what keeps it
//! out of a notification.

use lakko_contacts::key_rotation::{decode_key_rotation, KEY_ROTATION_ENVELOPE_LEN};

/// `true` when `plaintext` is a well-formed key-rotation envelope
/// (ADR-0074 section 3.3). The NSE suppresses it: no notification, no
/// preview, nothing stored.
///
/// Returns no field of the envelope. See the module docs for why.
#[uniffi::export]
#[must_use]
pub fn is_key_rotation_envelope(plaintext: Vec<u8>) -> bool {
    decode_key_rotation(&plaintext).is_ok()
}

/// The one length of a key-rotation envelope in bytes (192).
///
/// Derived from the core's constant, so a wire revision moves it here
/// automatically. Shape-parallel to `pin_envelope_len`.
#[uniffi::export]
#[must_use]
pub fn key_rotation_envelope_len() -> u32 {
    u32::try_from(KEY_ROTATION_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::key_rotation::{
        compose_key_rotation, KeyRotation, KEY_ROTATION_ENVELOPE_DOMAIN_V1,
    };

    fn wire() -> Vec<u8> {
        compose_key_rotation(&KeyRotation {
            prepared_at_ms: 1_700_000_000_000,
            new_entropy: [0xE7; 16],
            supersession_proof: [0x5A; 145],
        })
    }

    #[test]
    fn recognises_a_well_formed_envelope_and_nothing_else() {
        assert!(is_key_rotation_envelope(wire()));
        assert!(!is_key_rotation_envelope(Vec::new()));
        assert!(!is_key_rotation_envelope(b"hello".to_vec()));
        assert!(!is_key_rotation_envelope(wire()[..191].to_vec()));
        let mut long = wire();
        long.push(0);
        assert!(!is_key_rotation_envelope(long));
        let mut v2 = wire();
        v2[KEY_ROTATION_ENVELOPE_DOMAIN_V1.len()] = 0x02;
        assert!(!is_key_rotation_envelope(v2));
    }

    #[test]
    fn sibling_envelopes_are_not_key_rotations_and_the_reverse() {
        let closed = lakko_contacts::group_closed::compose_group_closed(
            &lakko_contacts::group_closed::GroupClosed {
                closed_at_ms: 1,
                successor_group_id: None,
            },
        );
        assert!(!is_key_rotation_envelope(closed));
        assert!(crate::group_closed::decode_group_closed_envelope(wire()).is_err());
        assert!(crate::pin::decode_pin_envelope(wire()).is_err());
    }

    /// Asserts the defect this arm exists to close, so the arm cannot be
    /// deleted silently: without it the chat parser would hand the NSE
    /// something to preview that is not a chat message.
    #[test]
    fn an_unfiltered_envelope_is_never_a_chat_body() {
        let parsed = crate::attachments::parse_body_envelope(wire());
        assert_eq!(
            parsed.kind,
            crate::attachments::MessageBodyKind::ServiceEnvelope,
            "a key rotation that slips past this arm must still not be chat text"
        );
    }

    /// Pins the shape the Swift pre-filter hardcodes, against LITERALS, so a
    /// wire revision fails Rust CI rather than a notification.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(key_rotation_envelope_len(), 192);
        assert_eq!(wire().len(), 192);
        assert_eq!(wire()[0], 0x6c, "the envelope opens with 'l' of lakko/");
        assert_eq!(&wire()[..22], b"lakko/key-rotation/v1\x00");
    }
}
