// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `lakko_channels::classify_post_plaintext` (ADR-0051 §4).
//!
//! The channel metadata record rides as the plaintext of an ordinary
//! post, so every reader runs this classifier on every decrypted post
//! **before** the text path. The bytes come from a signed post, but the
//! signer is any publisher — a malicious co-publisher, or a publisher
//! key that leaked — so the parser must treat them as hostile: the
//! one-byte and two-byte length prefixes, the version byte and the
//! UTF-8 / bound checks all run on attacker-chosen input. Since schema
//! version 2 (ADR-0059 §2) that includes the topic byte, the language
//! length prefix and code; since version 3 (ADR-0087) the public flag and
//! the length-prefixed conversation-handle envelope; the photo field
//! (ADR-0087 section 18.2: a length byte admitting 0, 84 or 170 and the
//! square / size / duration bounds of `lakko_contacts::avatar`); and the allowed-reactions
//! mode with its bounded list (count and every emoji length checked before a
//! copy). A plaintext that is not
//! a record goes to the chat-envelope parser (`lakko_attachments`).
//!
//! Three properties are asserted, not merely exercised:
//!
//! 1. The classifier never panics.
//! 2. A `Meta` result is within every documented bound (so the record
//!    the reader stores is one `compose` would have produced) and
//!    re-composes to the identical bytes — the encoding is canonical.
//! 3. A `Message` result carries the input verbatim beside its body.
//!
//! Since ADR-0087 section 13a the classifier also recognises the ban list
//! (its own domain, checked first); a `BanList` result must re-compose to the
//! input.

#![no_main]

use lakko_channels::{
    classify_post_plaintext, compose_channel_meta, PostContent, CHANNEL_META_MAX_LEN,
    CHANNEL_META_MIN_LEN,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    match classify_post_plaintext(data) {
        PostContent::Message { plaintext, .. } => {
            assert_eq!(plaintext, data, "a Message carries the input verbatim");
        },
        PostContent::NotAPost => {},
        PostContent::Meta(meta) => {
            assert!(
                data.len() >= CHANNEL_META_MIN_LEN && data.len() <= CHANNEL_META_MAX_LEN,
                "a decoded record must lie within the documented length bounds"
            );
            assert!(
                meta.validate().is_ok(),
                "a decoded record must pass validate"
            );
            assert!(
                meta.language.is_empty() || (2..=3).contains(&meta.language.len()),
                "a decoded language is unset or an ISO 639 code"
            );
            if let Some(avatar) = &meta.avatar {
                assert!(
                    avatar.validate().is_ok(),
                    "a decoded photo is within every avatar bound"
                );
                assert_eq!(avatar.still.width, avatar.still.height);
            }
            let recomposed = compose_channel_meta(&meta).expect("a decoded record must re-compose");
            assert_eq!(recomposed, data, "the encoding must be canonical");
        },
        PostContent::Malformed(_) | PostContent::MalformedBanList(_) => {},
        PostContent::BanList(list) => {
            // The ban list has its own target (`channel_ban_list_decode`);
            // here it is enough that a recognised list re-composes.
            let recomposed = lakko_channels::compose_ban_list(&list)
                .expect("a decoded ban list must re-compose");
            assert_eq!(recomposed, data, "the ban-list encoding must be canonical");
        },
    }
});
