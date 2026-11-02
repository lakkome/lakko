// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The fixed list of reaction emoji a channel post takes
//! ([ADR-0087] section 13a).
//!
//! A reaction under a channel post is a row at the channel service holding a
//! 16-byte label, `BLAKE3-keyed(reaction_key, post ‖ emoji)`, so the service
//! holds no emoji. A reader turns a label back into an emoji by computing the
//! label of every emoji in this list for the post and looking the row's label
//! up; a label that matches none is dropped. So the list is closed: it is
//! the whole set of reactions a channel post can show, and every client draws
//! its reaction picker for a channel from it, in this order.
//!
//! The 64 emoji are **chosen, not measured** (the ADR says so): common
//! approval, emotion and celebration faces and gestures, and a few objects,
//! all old enough (Unicode 13 or earlier) to render on every supported
//! platform. Whether the list should grow is ADR-0087 open question 6.
//!
//! ## What is frozen
//!
//! An emoji's **bytes** are what a label hashes, so an entry's spelling never
//! changes: `"❤️"` is U+2764 U+FE0F, and a client that sent U+2764 alone would
//! compute a label nobody can read. [`channel_reaction_index`] is an exact
//! byte match for that reason. An entry is never removed (a reaction already
//! given would stop resolving); a later list may append. The order decides
//! only where an emoji sits in the picker.
//!
//! Chat reactions (ADR-0023) are not bound by this list: a group member's
//! reaction travels inside the MLS group and names its emoji in the clear to
//! the group.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

/// How many emoji a channel post can be reacted with.
pub const CHANNEL_REACTION_EMOJI_COUNT: usize = 64;

/// The reaction emoji of a channel post, in picker order. See the module
/// docs for what is frozen.
pub const CHANNEL_REACTION_EMOJI: [&str; CHANNEL_REACTION_EMOJI_COUNT] = [
    "\u{1F44D}",        // thumbs up
    "\u{2764}\u{FE0F}", // red heart
    "\u{1F602}",        // face with tears of joy
    "\u{1F62E}",        // face with open mouth
    "\u{1F622}",        // crying face
    "\u{1F64F}",        // folded hands
    "\u{1F525}",        // fire
    "\u{1F44E}",        // thumbs down
    "\u{1F389}",        // party popper
    "\u{1F44F}",        // clapping hands
    "\u{1F601}",        // beaming face
    "\u{1F914}",        // thinking face
    "\u{1F92F}",        // exploding head
    "\u{1F631}",        // face screaming in fear
    "\u{1F621}",        // pouting face
    "\u{1F970}",        // smiling face with hearts
    "\u{1F60D}",        // smiling face with heart-eyes
    "\u{1F929}",        // star-struck
    "\u{1F60E}",        // smiling face with sunglasses
    "\u{1F973}",        // partying face
    "\u{1F62D}",        // loudly crying face
    "\u{1F605}",        // grinning face with sweat
    "\u{1F642}",        // slightly smiling face
    "\u{1F609}",        // winking face
    "\u{1F634}",        // sleeping face
    "\u{1F917}",        // smiling face with open hands
    "\u{1F91D}",        // handshake
    "\u{1F4AA}",        // flexed biceps
    "\u{1F44C}",        // OK hand
    "\u{270C}\u{FE0F}", // victory hand
    "\u{1F91E}",        // crossed fingers
    "\u{1F440}",        // eyes
    "\u{1F4AF}",        // hundred points
    "\u{2705}",         // check mark button
    "\u{274C}",         // cross mark
    "\u{2B50}",         // star
    "\u{2728}",         // sparkles
    "\u{26A1}",         // high voltage
    "\u{1F4A1}",        // light bulb
    "\u{1F680}",        // rocket
    "\u{1F3C6}",        // trophy
    "\u{1F3AF}",        // direct hit
    "\u{1F494}",        // broken heart
    "\u{1F4A9}",        // pile of poo
    "\u{1F921}",        // clown face
    "\u{1F648}",        // see-no-evil monkey
    "\u{1F607}",        // smiling face with halo
    "\u{1F923}",        // rolling on the floor laughing
    "\u{1F972}",        // smiling face with tear
    "\u{1F62C}",        // grimacing face
    "\u{1F928}",        // face with raised eyebrow
    "\u{1F610}",        // neutral face
    "\u{1F913}",        // nerd face
    "\u{1F937}",        // person shrugging
    "\u{1F480}",        // skull
    "\u{1F47B}",        // ghost
    "\u{1F339}",        // rose
    "\u{1F340}",        // four leaf clover
    "\u{2615}",         // hot beverage
    "\u{1F355}",        // pizza
    "\u{1F3B5}",        // musical note
    "\u{1F37E}",        // bottle with popping cork
    "\u{1F433}",        // spouting whale
    "\u{1F31A}",        // new moon face
];

/// The position of `emoji` in [`CHANNEL_REACTION_EMOJI`], or `None` when a
/// channel post does not take it. An exact byte match: a different spelling
/// of the same picture (a missing U+FE0F) is not in the list.
#[must_use]
pub fn channel_reaction_index(emoji: &str) -> Option<usize> {
    CHANNEL_REACTION_EMOJI.iter().position(|e| *e == emoji)
}

/// `true` when a channel post takes `emoji` as a reaction.
#[must_use]
pub fn is_channel_reaction_emoji(emoji: &str) -> bool {
    channel_reaction_index(emoji).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_has_64_distinct_entries() {
        let mut sorted = CHANNEL_REACTION_EMOJI.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), CHANNEL_REACTION_EMOJI_COUNT);
    }

    /// Every entry is a short, non-ASCII, whitespace-free string that fits
    /// the chat reaction bound (`lakko_attachments::MAX_EMOJI_BYTES`, 64), so
    /// a channel reaction is always also a valid chat reaction.
    #[test]
    fn every_entry_is_a_plausible_single_emoji() {
        for e in CHANNEL_REACTION_EMOJI {
            assert!(!e.is_empty());
            assert!(e.len() <= 16, "{e:?}");
            assert!(e.chars().count() <= 2, "{e:?}");
            assert!(!e.is_ascii(), "{e:?}");
            assert!(!e.chars().any(char::is_whitespace), "{e:?}");
        }
    }

    #[test]
    fn lookup_is_an_exact_byte_match() {
        assert_eq!(channel_reaction_index("\u{1F44D}"), Some(0));
        assert_eq!(channel_reaction_index("\u{2764}\u{FE0F}"), Some(1));
        // The heart without its variation selector is another string.
        assert_eq!(channel_reaction_index("\u{2764}"), None);
        assert!(!is_channel_reaction_emoji(""));
        assert!(!is_channel_reaction_emoji("+1"));
        for (i, e) in CHANNEL_REACTION_EMOJI.iter().enumerate() {
            assert_eq!(channel_reaction_index(e), Some(i));
        }
    }

    /// The list opens with the six chat reactions every client already
    /// offers (ADR-0023), in their order.
    #[test]
    fn the_list_opens_with_the_six_chat_reactions() {
        assert_eq!(
            CHANNEL_REACTION_EMOJI[..6],
            [
                "\u{1F44D}",
                "\u{2764}\u{FE0F}",
                "\u{1F602}",
                "\u{1F62E}",
                "\u{1F622}",
                "\u{1F64F}",
            ]
        );
        assert_eq!(CHANNEL_REACTION_EMOJI[63], "\u{1F31A}");
    }
}
