// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The preset feeds (UX spec `docs/ux/feed.md` section 4).
//!
//! Constants, so "Popular" means the same thing on every client. The names
//! here are English defaults; a client shows its own localised title for a
//! preset and never the stored name. Discover is ADR-0059's and is not here.

use crate::rule::{FeedRule, HalfLife, Suggestions, Weights, MAX_WEIGHT, MAX_WINDOW_DAYS};

/// A built-in feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Preset {
    /// Every group and channel, newest first.
    Latest,
    /// The last three days, group messages ranked by reactions and replies.
    Popular,
    /// Mentions, replies to the person, and their own messages that drew a
    /// reaction.
    ForMe,
}

/// Every preset, in the order the feed switcher shows them.
pub const ALL_PRESETS: [Preset; 3] = [Preset::Latest, Preset::Popular, Preset::ForMe];

/// The rule behind a preset.
#[must_use]
pub fn preset_rule(preset: Preset) -> FeedRule {
    match preset {
        Preset::Latest => FeedRule::new("Latest"),
        Preset::Popular => FeedRule {
            weights: Weights {
                recency: 30,
                reactions: MAX_WEIGHT,
                replies: 80,
                from_contacts: 20,
                mentions_me: 0,
                has_media: 10,
            },
            half_life: HalfLife::Day,
            window_days: 3,
            ..FeedRule::new("Popular")
        },
        Preset::ForMe => FeedRule {
            channels: false,
            for_me: true,
            window_days: MAX_WINDOW_DAYS,
            suggestions: Suggestions::Never,
            ..FeedRule::new("For me")
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{decode_feed_rule, encode_feed_rule};

    #[test]
    fn every_preset_is_a_valid_rule_that_round_trips() {
        for p in ALL_PRESETS {
            let rule = preset_rule(p);
            let bytes = encode_feed_rule(&rule).unwrap();
            assert_eq!(decode_feed_rule(&bytes).unwrap(), rule, "{p:?}");
        }
    }
}
