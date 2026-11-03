// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The quick reaction row, one definition for chats and channels.
//!
//! A message's reaction menu opens with a short row of emoji a person can
//! press at once, and a full picker behind it. Any single emoji is a valid
//! reaction, in a chat (ADR-0023, ADR-0047) and under a channel post
//! (ADR-0087 section 13a); this row only decides what is offered first. The
//! three clients each spelled the six chat reactions themselves; this is the
//! one spelling, which a channel's composer uses too.
//!
//! An entry's bytes matter where a reaction is hashed (a channel reaction
//! label is `BLAKE3-keyed(key, post ‖ emoji)`), so `"❤️"` is U+2764 U+FE0F
//! and a client offers exactly these strings.
//!
//! A channel owner may narrow what its posts take (the allowed-reactions field
//! of the channel metadata, `lakko_channels::meta`); a client then drops from
//! this row what the channel does not allow.

/// The most reactions one person gives one message, in a chat and under a
/// channel post (maintainer, 2026-11-02). A client rule: under a channel post
/// nothing at the service can check it (every reaction has its own write
/// key), and `lakko_sync` refuses a fourth own reaction on one post.
pub const MAX_REACTIONS_PER_MESSAGE: usize = 3;

/// The quick reaction row, in order.
pub const QUICK_REACTIONS: [&str; 6] = [
    "\u{1F44D}",        // thumbs up
    "\u{2764}\u{FE0F}", // red heart
    "\u{1F602}",        // face with tears of joy
    "\u{1F62E}",        // face with open mouth
    "\u{1F622}",        // crying face
    "\u{1F64F}",        // folded hands
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_per_message_limit_is_the_one_the_sync_register_enforces() {
        assert_eq!(
            MAX_REACTIONS_PER_MESSAGE,
            lakko_sync::synced::CHANNEL_REACTIONS_PER_POST_MAX
        );
    }

    #[test]
    fn the_row_is_six_distinct_emoji_in_the_chat_order() {
        let mut sorted = QUICK_REACTIONS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 6);
        assert_eq!(QUICK_REACTIONS[0], "\u{1F44D}");
        // The heart carries its variation selector.
        assert_eq!(QUICK_REACTIONS[1].len(), 6);
        for e in QUICK_REACTIONS {
            assert!(!e.is_empty() && e.len() <= 64 && !e.is_ascii(), "{e:?}");
        }
    }
}
