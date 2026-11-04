// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim CLASSIFY surface for the ADR-0087 service envelopes that ride a
//! group journal: the group photo (section 18.1), the group's allowed
//! reactions (section 13a), the conversation handle (section 3), the
//! knock-seed envelope (section 5) and, in a channel's PublisherGroup, the
//! admission-seed envelope (section 7).
//!
//! ## Why the NSE needs this
//!
//! Each is an ordinary MLS application message, and any of them can be the
//! newest row when a push wake arrives. `parse_body_envelope` already
//! classifies a `lakko/` plaintext as a never-rendered `ServiceEnvelope`, so
//! these arms are the explicit, length-pinned recognition the NSE's
//! pre-filter keys on, in the [`crate::link_claim`] shape.
//!
//! ## Scope: recognise + the length bounds, nothing else (C-59)
//!
//! No compose, no open, no order comparison, no role gate: the NSE takes no
//! acceptance decision and stores nothing. A sealed seed stays sealed here.
//! Every recogniser is fail-closed: a malformed envelope with the right
//! separator answers `false`, and the chain falls through.

use lakko_contacts::admission_seed::{decode_admission_seed, ADMISSION_SEED_ENVELOPE_LEN};
use lakko_contacts::conversation_handle::{
    decode_conversation_handle, CONVERSATION_HANDLE_ENVELOPE_MAX_LEN,
    CONVERSATION_HANDLE_ENVELOPE_MIN_LEN,
};
use lakko_contacts::group_photo::{
    decode_group_photo, GROUP_PHOTO_ENVELOPE_MAX_LEN, GROUP_PHOTO_ENVELOPE_MIN_LEN,
};
use lakko_contacts::group_reactions::{
    decode_group_reactions, GROUP_REACTIONS_ENVELOPE_MAX_LEN, GROUP_REACTIONS_ENVELOPE_MIN_LEN,
};
use lakko_contacts::knock_seed::{decode_knock_seed, KNOCK_SEED_ENVELOPE_LEN};

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Whether `plaintext` is a well-formed group-photo envelope.
#[uniffi::export]
#[must_use]
pub fn is_group_photo_envelope(plaintext: Vec<u8>) -> bool {
    decode_group_photo(&plaintext).is_ok()
}

/// Shortest group-photo envelope (a removal).
#[uniffi::export]
#[must_use]
pub fn group_photo_envelope_min_len() -> u32 {
    len_u32(GROUP_PHOTO_ENVELOPE_MIN_LEN)
}

/// Longest group-photo envelope (a photo with a video).
#[uniffi::export]
#[must_use]
pub fn group_photo_envelope_max_len() -> u32 {
    len_u32(GROUP_PHOTO_ENVELOPE_MAX_LEN)
}

/// Whether `plaintext` is a well-formed group allowed-reactions envelope.
#[uniffi::export]
#[must_use]
pub fn is_group_reactions_envelope(plaintext: Vec<u8>) -> bool {
    decode_group_reactions(&plaintext).is_ok()
}

/// Shortest group allowed-reactions envelope.
#[uniffi::export]
#[must_use]
pub fn group_reactions_envelope_min_len() -> u32 {
    len_u32(GROUP_REACTIONS_ENVELOPE_MIN_LEN)
}

/// Longest group allowed-reactions envelope.
#[uniffi::export]
#[must_use]
pub fn group_reactions_envelope_max_len() -> u32 {
    len_u32(GROUP_REACTIONS_ENVELOPE_MAX_LEN)
}

/// Whether `plaintext` is a well-formed conversation-handle envelope (its
/// signature is not checked here; nothing is shown from it).
#[uniffi::export]
#[must_use]
pub fn is_conversation_handle_envelope(plaintext: Vec<u8>) -> bool {
    decode_conversation_handle(&plaintext).is_ok()
}

/// Shortest conversation-handle envelope.
#[uniffi::export]
#[must_use]
pub fn conversation_handle_envelope_min_len() -> u32 {
    len_u32(CONVERSATION_HANDLE_ENVELOPE_MIN_LEN)
}

/// Longest conversation-handle envelope.
#[uniffi::export]
#[must_use]
pub fn conversation_handle_envelope_max_len() -> u32 {
    len_u32(CONVERSATION_HANDLE_ENVELOPE_MAX_LEN)
}

/// Whether `plaintext` is a well-formed knock-seed envelope (the seed stays
/// sealed).
#[uniffi::export]
#[must_use]
pub fn is_knock_seed_envelope(plaintext: Vec<u8>) -> bool {
    decode_knock_seed(&plaintext).is_ok()
}

/// The one length of a knock-seed envelope.
#[uniffi::export]
#[must_use]
pub fn knock_seed_envelope_len() -> u32 {
    len_u32(KNOCK_SEED_ENVELOPE_LEN)
}

/// Whether `plaintext` is a well-formed admission-seed envelope (the seed
/// stays sealed).
#[uniffi::export]
#[must_use]
pub fn is_admission_seed_envelope(plaintext: Vec<u8>) -> bool {
    decode_admission_seed(&plaintext).is_ok()
}

/// The one length of an admission-seed envelope.
#[uniffi::export]
#[must_use]
pub fn admission_seed_envelope_len() -> u32 {
    len_u32(ADMISSION_SEED_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::admission_seed::compose_admission_seed;
    use lakko_contacts::allowed_reactions::AllowedReactions;
    use lakko_contacts::conversation_handle::{compose_conversation_handle, ConversationHandle};
    use lakko_contacts::group_photo::{compose_group_photo, GroupPhoto};
    use lakko_contacts::group_reactions::{compose_group_reactions, GroupReactions};
    use lakko_contacts::knock_seed::compose_knock_seed;
    use lakko_crypto::agreement::SecretKey;

    fn photo() -> Vec<u8> {
        compose_group_photo(&GroupPhoto {
            avatar: None,
            updated_at_ms: 1,
        })
        .unwrap()
    }

    fn reactions() -> Vec<u8> {
        compose_group_reactions(&GroupReactions {
            reactions: AllowedReactions::Off,
            updated_at_ms: 1,
        })
        .unwrap()
    }

    fn handle() -> Vec<u8> {
        let h =
            ConversationHandle::sign(&[0x21; 32], &[0x42; 32], 1, true, "abc", [0; 169]).unwrap();
        compose_conversation_handle(&h).unwrap()
    }

    fn knock_seed() -> Vec<u8> {
        let pk = SecretKey::from_bytes([0x33; 32]).public_key();
        compose_knock_seed(&[0x4A; 32], &[0x4B; 32], &pk, 1, &[0x11; 32]).unwrap()
    }

    fn admission_seed() -> Vec<u8> {
        let pk = SecretKey::from_bytes([0x33; 32]).public_key();
        compose_admission_seed(&[0x0C; 16], &[0x4B; 32], &pk, 1, &[0x11; 32]).unwrap()
    }

    #[test]
    fn recognises_each_envelope_and_refuses_chat() {
        assert!(is_group_photo_envelope(photo()));
        assert!(is_group_reactions_envelope(reactions()));
        assert!(is_conversation_handle_envelope(handle()));
        assert!(is_knock_seed_envelope(knock_seed()));
        assert!(is_admission_seed_envelope(admission_seed()));
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(!is_group_photo_envelope(chat.clone()));
        assert!(!is_group_reactions_envelope(chat.clone()));
        assert!(!is_conversation_handle_envelope(chat.clone()));
        assert!(!is_knock_seed_envelope(chat.clone()));
        assert!(!is_admission_seed_envelope(chat));
    }

    #[test]
    fn no_envelope_is_ever_chat_text() {
        for w in [
            photo(),
            reactions(),
            handle(),
            knock_seed(),
            admission_seed(),
        ] {
            assert_eq!(
                lakko_attachments::parse_body(&w),
                lakko_attachments::MessageBody::ServiceEnvelope,
            );
        }
    }

    /// The Swift pre-filter keys on the length range and `first == 0x6c`;
    /// pinned as LITERALS so a wire change fails a Rust test.
    #[test]
    #[allow(clippy::many_single_char_names)]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(group_photo_envelope_min_len(), 31);
        assert_eq!(group_photo_envelope_max_len(), 201);
        assert_eq!(group_reactions_envelope_min_len(), 35);
        assert_eq!(group_reactions_envelope_max_len(), 6536);
        assert_eq!(conversation_handle_envelope_min_len(), 276);
        assert_eq!(conversation_handle_envelope_max_len(), 305);
        assert_eq!(knock_seed_envelope_len(), 205);
        assert_eq!(admission_seed_envelope_len(), 195);
        let p = photo();
        assert_eq!(&p[..21], b"lakko/group-photo/v1\x00");
        assert_eq!(p.len(), 31);
        let r = reactions();
        assert_eq!(&r[..25], b"lakko/group-reactions/v1\x00");
        assert_eq!(r.len(), 35);
        let h = handle();
        assert_eq!(&h[..29], b"lakko/conversation-handle/v1\x00");
        assert_eq!(h.len(), 276);
        let k = knock_seed();
        assert_eq!(&k[..26], b"lakko/group-knock-seed/v1\x00");
        assert_eq!(k.len(), 205);
        let a = admission_seed();
        assert_eq!(&a[..32], b"lakko/channel-admission-seed/v1\x00");
        assert_eq!(a.len(), 195);
        for w in [p, r, h, k, a] {
            assert_eq!(w[0], 0x6c);
        }
    }

    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let all = [
            photo(),
            reactions(),
            handle(),
            knock_seed(),
            admission_seed(),
        ];
        let tests: [fn(Vec<u8>) -> bool; 5] = [
            is_group_photo_envelope,
            is_group_reactions_envelope,
            is_conversation_handle_envelope,
            is_knock_seed_envelope,
            is_admission_seed_envelope,
        ];
        for (i, w) in all.iter().enumerate() {
            for (j, t) in tests.iter().enumerate() {
                assert_eq!(t(w.clone()), i == j, "envelope {i} under recogniser {j}");
            }
            assert!(crate::group_name::decode_group_name_envelope(w.clone()).is_err());
        }
    }
}
