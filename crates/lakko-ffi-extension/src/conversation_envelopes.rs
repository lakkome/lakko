// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim CLASSIFY surface for the ADR-0087 service envelopes that ride a
//! group journal: the group photo (section 18.1), the group's allowed
//! reactions (section 13a), the conversation handle (section 3), the
//! knock-seed envelope (section 5) and, in a channel's PublisherGroup, the
//! admission-seed envelope (section 7), the read-key envelope (section 7,
//! phase B), the earlier-seeds envelope to a new publisher (section 7,
//! amended 2026-11-08), a publisher's request that the owner rotate
//! (section 15, amended 2026-11-08), a co-publisher's step-down (section
//! 15, phase B), and the ADR-0090 publisher bookkeeping: a co-publisher
//! device's admit / retire request and a publishing device's admission mark,
//! and ADR-0091 phase 2: a device's request to come back into a conversation
//! (route 1, a DeviceGroup service envelope) and the group's knock key handed
//! to members.
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
use lakko_contacts::channel_admission_mark::{
    decode_channel_admission_mark, CHANNEL_ADMISSION_MARK_ENVELOPE_LEN,
};
use lakko_contacts::channel_epoch_seeds::{
    decode_channel_epoch_seeds, CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN,
    CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN,
};
use lakko_contacts::channel_read_key::{decode_channel_read_key, CHANNEL_READ_KEY_ENVELOPE_LEN};
use lakko_contacts::channel_rotate_request::{
    decode_channel_rotate_request, CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN,
};
use lakko_contacts::conversation_handle::{
    decode_conversation_handle, CONVERSATION_HANDLE_ENVELOPE_MAX_LEN,
    CONVERSATION_HANDLE_ENVELOPE_MIN_LEN,
};
use lakko_contacts::conversation_reentry::{
    decode_conversation_reentry, CONVERSATION_REENTRY_ENVELOPE_MAX_LEN,
    CONVERSATION_REENTRY_ENVELOPE_MIN_LEN,
};
use lakko_contacts::group_photo::{
    decode_group_photo, GROUP_PHOTO_ENVELOPE_MAX_LEN, GROUP_PHOTO_ENVELOPE_MIN_LEN,
};
use lakko_contacts::group_reactions::{
    decode_group_reactions, GROUP_REACTIONS_ENVELOPE_MAX_LEN, GROUP_REACTIONS_ENVELOPE_MIN_LEN,
};
use lakko_contacts::knock_key::{decode_knock_key, KNOCK_KEY_ENVELOPE_LEN};
use lakko_contacts::knock_seed::{decode_knock_seed, KNOCK_SEED_ENVELOPE_LEN};
use lakko_contacts::publisher_device_request::{
    decode_publisher_device_request, PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN,
    PUBLISHER_DEVICE_REQUEST_ENVELOPE_MIN_LEN,
};

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

/// Whether `plaintext` is a well-formed channel read-key envelope (the key
/// stays sealed).
#[uniffi::export]
#[must_use]
pub fn is_channel_read_key_envelope(plaintext: Vec<u8>) -> bool {
    decode_channel_read_key(&plaintext).is_ok()
}

/// The one length of a channel read-key envelope.
#[uniffi::export]
#[must_use]
pub fn channel_read_key_envelope_len() -> u32 {
    len_u32(CHANNEL_READ_KEY_ENVELOPE_LEN)
}

/// Whether `plaintext` is a well-formed earlier-seeds envelope to a new
/// publisher (the seeds stay sealed).
#[uniffi::export]
#[must_use]
pub fn is_channel_epoch_seeds_envelope(plaintext: Vec<u8>) -> bool {
    decode_channel_epoch_seeds(&plaintext).is_ok()
}

/// The shortest earlier-seeds envelope (one seed).
#[uniffi::export]
#[must_use]
pub fn channel_epoch_seeds_envelope_min_len() -> u32 {
    len_u32(CHANNEL_EPOCH_SEEDS_ENVELOPE_MIN_LEN)
}

/// The longest earlier-seeds envelope (64 seeds).
#[uniffi::export]
#[must_use]
pub fn channel_epoch_seeds_envelope_max_len() -> u32 {
    len_u32(CHANNEL_EPOCH_SEEDS_ENVELOPE_MAX_LEN)
}

/// Whether `plaintext` is a publisher's request that the owner rotate (any
/// channel).
#[uniffi::export]
#[must_use]
pub fn is_channel_rotate_request_envelope(plaintext: Vec<u8>) -> bool {
    decode_channel_rotate_request(&plaintext).is_ok()
}

/// The one length of a rotate-request envelope.
#[uniffi::export]
#[must_use]
pub fn channel_rotate_request_envelope_len() -> u32 {
    len_u32(CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN)
}

/// Whether `plaintext` is a co-publisher's step-down envelope (any channel).
#[uniffi::export]
#[must_use]
pub fn is_publisher_step_down_envelope(plaintext: Vec<u8>) -> bool {
    lakko_mls::is_publisher_step_down_envelope(&plaintext)
}

/// The one length of a step-down envelope.
#[uniffi::export]
#[must_use]
pub fn publisher_step_down_envelope_len() -> u32 {
    len_u32(lakko_mls::PUBLISHER_STEP_DOWN_LEN)
}

/// Whether `plaintext` is a co-publisher device's admit / retire request
/// (ADR-0090 section 2.2, any channel).
#[uniffi::export]
#[must_use]
pub fn is_publisher_device_request_envelope(plaintext: Vec<u8>) -> bool {
    decode_publisher_device_request(&plaintext).is_ok()
}

/// The shortest device-request envelope (a retire).
#[uniffi::export]
#[must_use]
pub fn publisher_device_request_envelope_min_len() -> u32 {
    len_u32(PUBLISHER_DEVICE_REQUEST_ENVELOPE_MIN_LEN)
}

/// The longest device-request envelope.
#[uniffi::export]
#[must_use]
pub fn publisher_device_request_envelope_max_len() -> u32 {
    len_u32(PUBLISHER_DEVICE_REQUEST_ENVELOPE_MAX_LEN)
}

/// Whether `plaintext` is a publishing device's admission mark (ADR-0090
/// section 6, any channel).
#[uniffi::export]
#[must_use]
pub fn is_channel_admission_mark_envelope(plaintext: Vec<u8>) -> bool {
    decode_channel_admission_mark(&plaintext).is_ok()
}

/// The one length of an admission-mark envelope.
#[uniffi::export]
#[must_use]
pub fn channel_admission_mark_envelope_len() -> u32 {
    len_u32(CHANNEL_ADMISSION_MARK_ENVELOPE_LEN)
}

/// Whether `plaintext` is a request to be brought back into a conversation
/// (ADR-0091 phase 2, any conversation).
#[uniffi::export]
#[must_use]
pub fn is_conversation_reentry_envelope(plaintext: Vec<u8>) -> bool {
    decode_conversation_reentry(&plaintext).is_ok()
}

/// The shortest re-entry request envelope.
#[uniffi::export]
#[must_use]
pub fn conversation_reentry_envelope_min_len() -> u32 {
    len_u32(CONVERSATION_REENTRY_ENVELOPE_MIN_LEN)
}

/// The longest re-entry request envelope.
#[uniffi::export]
#[must_use]
pub fn conversation_reentry_envelope_max_len() -> u32 {
    len_u32(CONVERSATION_REENTRY_ENVELOPE_MAX_LEN)
}

/// Whether `plaintext` is a group's knock-key envelope (ADR-0091 phase 2).
#[uniffi::export]
#[must_use]
pub fn is_knock_key_envelope(plaintext: Vec<u8>) -> bool {
    decode_knock_key(&plaintext).is_ok()
}

/// The one length of a knock-key envelope.
#[uniffi::export]
#[must_use]
pub fn knock_key_envelope_len() -> u32 {
    len_u32(KNOCK_KEY_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::admission_seed::compose_admission_seed;
    use lakko_contacts::allowed_reactions::AllowedReactions;
    use lakko_contacts::channel_epoch_seeds::{compose_channel_epoch_seeds, ChannelEpochSeed};
    use lakko_contacts::channel_read_key::compose_channel_read_key;
    use lakko_contacts::channel_rotate_request::compose_channel_rotate_request;
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

    fn read_key() -> Vec<u8> {
        let pk = SecretKey::from_bytes([0x33; 32]).public_key();
        compose_channel_read_key(&[0x0C; 16], &[0x4B; 32], &pk, &[0x11; 32]).unwrap()
    }

    fn epoch_seeds() -> Vec<u8> {
        let pk = SecretKey::from_bytes([0x33; 32]).public_key();
        let seeds = [ChannelEpochSeed {
            epoch: 0,
            seed: [0x11; 32],
        }];
        compose_channel_epoch_seeds(&[0x0C; 16], &[0x4B; 32], &pk, &seeds).unwrap()
    }

    fn rotate_request() -> Vec<u8> {
        compose_channel_rotate_request(&[0x0C; 16])
    }

    fn step_down() -> Vec<u8> {
        let channel = lakko_mls::ChannelId::try_from_slice(&[0x0C; 16]).unwrap();
        lakko_mls::compose_publisher_step_down(&channel)
    }

    fn device_request() -> Vec<u8> {
        use lakko_contacts::publisher_device_request::{
            compose_publisher_device_request, PublisherDeviceAction, PublisherDeviceRequest,
        };
        compose_publisher_device_request(&PublisherDeviceRequest {
            channel_id: [0x0C; 16],
            action: PublisherDeviceAction::Retire {
                device_key: [0x7E; 32],
            },
        })
        .unwrap()
    }

    fn admission_mark() -> Vec<u8> {
        lakko_contacts::channel_admission_mark::compose_channel_admission_mark(
            &lakko_contacts::channel_admission_mark::ChannelAdmissionMark {
                channel_id: [0x0C; 16],
                through_seq: 7,
            },
        )
    }

    fn reentry() -> Vec<u8> {
        lakko_contacts::conversation_reentry::compose_conversation_reentry(
            &lakko_contacts::conversation_reentry::ConversationReentry {
                group_id: [0x0D; 32],
                epoch_left: 16,
                gap_from_ms: 1,
                gap_key: [0x6B; 32],
                key_package: vec![0x55],
            },
        )
        .unwrap()
    }

    fn knock_key() -> Vec<u8> {
        lakko_contacts::knock_key::compose_knock_key(&lakko_contacts::knock_key::KnockKey {
            knock_version: 2,
            knock_pk: [0x4B; 32],
        })
    }

    #[test]
    fn recognises_each_envelope_and_refuses_chat() {
        assert!(is_conversation_reentry_envelope(reentry()));
        assert!(is_knock_key_envelope(knock_key()));
        assert!(is_group_photo_envelope(photo()));
        assert!(is_group_reactions_envelope(reactions()));
        assert!(is_conversation_handle_envelope(handle()));
        assert!(is_knock_seed_envelope(knock_seed()));
        assert!(is_admission_seed_envelope(admission_seed()));
        assert!(is_channel_read_key_envelope(read_key()));
        assert!(is_channel_epoch_seeds_envelope(epoch_seeds()));
        assert!(is_channel_rotate_request_envelope(rotate_request()));
        assert!(is_publisher_step_down_envelope(step_down()));
        assert!(is_publisher_device_request_envelope(device_request()));
        assert!(is_channel_admission_mark_envelope(admission_mark()));
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(!is_group_photo_envelope(chat.clone()));
        assert!(!is_group_reactions_envelope(chat.clone()));
        assert!(!is_conversation_handle_envelope(chat.clone()));
        assert!(!is_knock_seed_envelope(chat.clone()));
        assert!(!is_admission_seed_envelope(chat.clone()));
        assert!(!is_channel_read_key_envelope(chat.clone()));
        assert!(!is_channel_epoch_seeds_envelope(chat.clone()));
        assert!(!is_channel_rotate_request_envelope(chat.clone()));
        assert!(!is_publisher_device_request_envelope(chat.clone()));
        assert!(!is_channel_admission_mark_envelope(chat.clone()));
        assert!(!is_conversation_reentry_envelope(chat.clone()));
        assert!(!is_knock_key_envelope(chat.clone()));
        assert!(!is_publisher_step_down_envelope(chat));
    }

    #[test]
    fn no_envelope_is_ever_chat_text() {
        for w in [
            photo(),
            reactions(),
            handle(),
            knock_seed(),
            admission_seed(),
            read_key(),
            epoch_seeds(),
            rotate_request(),
            step_down(),
            device_request(),
            admission_mark(),
            reentry(),
            knock_key(),
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
        assert_eq!(channel_read_key_envelope_len(), 155);
        assert_eq!(channel_epoch_seeds_envelope_min_len(), 163);
        assert_eq!(channel_epoch_seeds_envelope_max_len(), 2431);
        assert_eq!(channel_rotate_request_envelope_len(), 49);
        assert_eq!(publisher_step_down_envelope_len(), 46);
        assert_eq!(publisher_device_request_envelope_min_len(), 84);
        assert_eq!(publisher_device_request_envelope_max_len(), 7208);
        assert_eq!(channel_admission_mark_envelope_len(), 57);
        // Version 2 carries the 32-byte gap key (security review 2026-11-21).
        assert_eq!(conversation_reentry_envelope_min_len(), 114);
        assert_eq!(conversation_reentry_envelope_max_len(), 4209);
        assert_eq!(knock_key_envelope_len(), 60);
        let re = reentry();
        assert_eq!(&re[..30], b"lakko/conversation-reentry/v1\x00");
        assert_eq!(re.len(), 114);
        assert_eq!(re[30], 0x02);
        let kk = knock_key();
        assert_eq!(&kk[..25], b"lakko/group-knock-key/v1\x00");
        assert_eq!(kk.len(), 60);
        assert_eq!(re[0], 0x6c);
        assert_eq!(kk[0], 0x6c);
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
        let c = read_key();
        assert_eq!(&c[..26], b"lakko/channel-read-key/v1\x00");
        assert_eq!(c.len(), 155);
        let e = epoch_seeds();
        assert_eq!(&e[..29], b"lakko/channel-epoch-seeds/v1\x00");
        assert_eq!(e.len(), 163);
        let q = rotate_request();
        assert_eq!(&q[..32], b"lakko/channel-rotate-request/v1\x00");
        assert_eq!(q.len(), 49);
        let d = step_down();
        assert_eq!(&d[..29], b"lakko/publisher-step-down/v1\x00");
        assert_eq!(d.len(), 46);
        let v = device_request();
        assert_eq!(&v[..34], b"lakko/publisher-device-request/v1\x00");
        assert_eq!(v.len(), 84);
        let m = admission_mark();
        assert_eq!(&m[..32], b"lakko/channel-admission-mark/v1\x00");
        assert_eq!(m.len(), 57);
        for w in [p, r, h, k, a, c, e, q, d, v, m] {
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
            read_key(),
            epoch_seeds(),
            rotate_request(),
            step_down(),
            device_request(),
            admission_mark(),
            reentry(),
            knock_key(),
        ];
        let tests: [fn(Vec<u8>) -> bool; 13] = [
            is_group_photo_envelope,
            is_group_reactions_envelope,
            is_conversation_handle_envelope,
            is_knock_seed_envelope,
            is_admission_seed_envelope,
            is_channel_read_key_envelope,
            is_channel_epoch_seeds_envelope,
            is_channel_rotate_request_envelope,
            is_publisher_step_down_envelope,
            is_publisher_device_request_envelope,
            is_channel_admission_mark_envelope,
            is_conversation_reentry_envelope,
            is_knock_key_envelope,
        ];
        for (i, w) in all.iter().enumerate() {
            for (j, t) in tests.iter().enumerate() {
                assert_eq!(t(w.clone()), i == j, "envelope {i} under recogniser {j}");
            }
            assert!(crate::group_name::decode_group_name_envelope(w.clone()).is_err());
        }
    }
}
