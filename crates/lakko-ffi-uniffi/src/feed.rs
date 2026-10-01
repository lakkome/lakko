// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The on-device feed (ADR-0063) — UniFFI surface.
//!
//! Mirror of `lakko-ffi-wasm/src/feed.rs` over the same [`lakko_feed`]
//! core. The ranking exists once so that the same messages are in the same
//! order on every phone; a client maps what it holds into these types, asks,
//! and draws the indices it gets back.
//!
//! Nothing here touches a screen, a store or the network.

use lakko_feed::{
    contiguous_seen as rust_contiguous_seen, decode_feed_rule as rust_decode_feed_rule,
    encode_feed_rule as rust_encode_feed_rule, mentions_username as rust_mentions_username,
    normalize_word as rust_normalize_word, preset_rule as rust_preset_rule, rank as rust_rank,
    ConversationKind as RustConversationKind, FeedCandidate as RustFeedCandidate,
    FeedConversation as RustFeedConversation, FeedRule as RustFeedRule,
    FeedRuleError as RustFeedRuleError, HalfLife as RustHalfLife, Preset as RustPreset,
    Suggestions as RustSuggestions, Weights as RustWeights,
};

use crate::FfiError;

fn feed_rule_error(e: RustFeedRuleError) -> FfiError {
    FfiError::FeedRule(format!("{}: {e}", e.code()))
}

/// How fast an item's recency fades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FeedHalfLife {
    /// Six hours.
    FewHours,
    /// One day.
    Day,
    /// One week.
    Week,
}

impl From<FeedHalfLife> for RustHalfLife {
    fn from(h: FeedHalfLife) -> Self {
        match h {
            FeedHalfLife::FewHours => Self::FewHours,
            FeedHalfLife::Day => Self::Day,
            FeedHalfLife::Week => Self::Week,
        }
    }
}

impl From<RustHalfLife> for FeedHalfLife {
    fn from(h: RustHalfLife) -> Self {
        match h {
            RustHalfLife::FewHours => Self::FewHours,
            RustHalfLife::Day => Self::Day,
            RustHalfLife::Week => Self::Week,
        }
    }
}

/// How often a suggested channel appears in a feed (ADR-0059).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FeedSuggestions {
    /// Never.
    Never,
    /// Rarely.
    Rarely,
    /// Sometimes.
    Sometimes,
}

impl From<FeedSuggestions> for RustSuggestions {
    fn from(s: FeedSuggestions) -> Self {
        match s {
            FeedSuggestions::Never => Self::Never,
            FeedSuggestions::Rarely => Self::Rarely,
            FeedSuggestions::Sometimes => Self::Sometimes,
        }
    }
}

impl From<RustSuggestions> for FeedSuggestions {
    fn from(s: RustSuggestions) -> Self {
        match s {
            RustSuggestions::Never => Self::Never,
            RustSuggestions::Rarely => Self::Rarely,
            RustSuggestions::Sometimes => Self::Sometimes,
        }
    }
}

/// How much each signal counts, each `0..=100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FeedWeights {
    /// How recent the message is.
    pub recency: u8,
    /// How many people reacted, relative to the group's size.
    pub reactions: u8,
    /// How many replies it has, relative to the group's size.
    pub replies: u8,
    /// Written by one of the person's contacts.
    pub from_contacts: u8,
    /// Mentions the person or replies to them.
    pub mentions_me: u8,
    /// Carries an attachment.
    pub has_media: u8,
}

impl From<FeedWeights> for RustWeights {
    fn from(w: FeedWeights) -> Self {
        Self {
            recency: w.recency,
            reactions: w.reactions,
            replies: w.replies,
            from_contacts: w.from_contacts,
            mentions_me: w.mentions_me,
            has_media: w.has_media,
        }
    }
}

impl From<RustWeights> for FeedWeights {
    fn from(w: RustWeights) -> Self {
        Self {
            recency: w.recency,
            reactions: w.reactions,
            replies: w.replies,
            from_contacts: w.from_contacts,
            mentions_me: w.mentions_me,
            has_media: w.has_media,
        }
    }
}

/// A feed rule: which messages it takes, what counts, what it filters out.
/// Field meanings and bounds are `lakko_feed::FeedRule`'s.
// Mirrors the core record field for field; the bools are independent
// switches the person sets.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FeedRule {
    /// The feed's name, under the display-name rule.
    pub name: String,
    /// Index into the client's fixed icon list.
    pub icon: u8,
    /// Take group messages.
    pub groups: bool,
    /// Take channel posts.
    pub channels: bool,
    /// Only these conversations, when non-empty. Never exported.
    pub include: Vec<Vec<u8>>,
    /// Never these conversations. Never exported.
    pub exclude: Vec<Vec<u8>>,
    /// Set by export when it removed include / exclude; the importer shows
    /// "Sources: all groups and channels".
    pub sources_stripped: bool,
    /// Take muted conversations too.
    pub include_muted: bool,
    /// What counts, and how much.
    pub weights: FeedWeights,
    /// How fast recency fades.
    pub half_life: FeedHalfLife,
    /// How far back the feed looks, `1..=30` days.
    pub window_days: u8,
    /// Only messages with an attachment.
    pub media_only: bool,
    /// Never the person's own messages.
    pub hide_own: bool,
    /// Only mentions, replies to the person, and their own messages that drew
    /// a reaction.
    pub for_me: bool,
    /// Only messages with at least this many people reacting.
    pub min_reactions: u16,
    /// Drop a message containing any of these (lowercase, whole words).
    pub hide_words: Vec<String>,
    /// When non-empty, keep only messages containing one of these.
    pub only_words: Vec<String>,
    /// How often suggestions appear.
    pub suggestions: FeedSuggestions,
}

impl From<FeedRule> for RustFeedRule {
    fn from(r: FeedRule) -> Self {
        Self {
            name: r.name,
            icon: r.icon,
            groups: r.groups,
            channels: r.channels,
            include: r.include,
            exclude: r.exclude,
            sources_stripped: r.sources_stripped,
            include_muted: r.include_muted,
            weights: r.weights.into(),
            half_life: r.half_life.into(),
            window_days: r.window_days,
            media_only: r.media_only,
            hide_own: r.hide_own,
            for_me: r.for_me,
            min_reactions: r.min_reactions,
            hide_words: r.hide_words,
            only_words: r.only_words,
            suggestions: r.suggestions.into(),
        }
    }
}

impl From<RustFeedRule> for FeedRule {
    fn from(r: RustFeedRule) -> Self {
        Self {
            name: r.name,
            icon: r.icon,
            groups: r.groups,
            channels: r.channels,
            include: r.include,
            exclude: r.exclude,
            sources_stripped: r.sources_stripped,
            include_muted: r.include_muted,
            weights: r.weights.into(),
            half_life: r.half_life.into(),
            window_days: r.window_days,
            media_only: r.media_only,
            hide_own: r.hide_own,
            for_me: r.for_me,
            min_reactions: r.min_reactions,
            hide_words: r.hide_words,
            only_words: r.only_words,
            suggestions: r.suggestions.into(),
        }
    }
}

/// A built-in feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FeedPreset {
    /// Every group and channel, newest first.
    Latest,
    /// The last three days, group messages ranked by reactions and replies.
    Popular,
    /// Mentions, replies to the person, their own reacted-to messages.
    ForMe,
}

impl From<FeedPreset> for RustPreset {
    fn from(p: FeedPreset) -> Self {
        match p {
            FeedPreset::Latest => Self::Latest,
            FeedPreset::Popular => Self::Popular,
            FeedPreset::ForMe => Self::ForMe,
        }
    }
}

/// Group or channel. One-to-one chats have no value here: they are never in
/// a feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FeedConversationKind {
    /// A group.
    Group,
    /// A channel.
    Channel,
}

impl From<FeedConversationKind> for RustConversationKind {
    fn from(k: FeedConversationKind) -> Self {
        match k {
            FeedConversationKind::Group => Self::Group,
            FeedConversationKind::Channel => Self::Channel,
        }
    }
}

/// One conversation the client offers messages from.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FeedConversation {
    /// The client's id for it, compared with the rule's include / exclude.
    pub id: Vec<u8>,
    /// Group or channel.
    pub kind: FeedConversationKind,
    /// Muted in this client.
    pub muted: bool,
    /// Number of members; ignored for a channel.
    pub audience: u32,
}

impl From<FeedConversation> for RustFeedConversation {
    fn from(c: FeedConversation) -> Self {
        Self {
            id: c.id,
            kind: c.kind.into(),
            muted: c.muted,
            audience: c.audience,
        }
    }
}

/// One message, reduced to what ranking needs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FeedCandidate {
    /// Index into the conversations list. Out of range = dropped.
    pub conversation: u32,
    /// The message id within its conversation.
    pub message_id: i64,
    /// The message it replies to, in the same conversation.
    pub reply_to: Option<i64>,
    /// When it was sent, unix milliseconds.
    pub time_ms: i64,
    /// Written by the person.
    pub from_self: bool,
    /// Written by one of the person's contacts.
    pub from_contact: bool,
    /// Mentions the person ([`mentions_username`]).
    pub mentions_me: bool,
    /// Replies to one of the person's messages.
    pub replies_to_me: bool,
    /// Distinct people who reacted.
    pub reactions_distinct: u32,
    /// Carries an attachment.
    pub has_media: bool,
    /// The text, for the word filters. Never logged.
    pub text: String,
}

impl From<FeedCandidate> for RustFeedCandidate {
    fn from(c: FeedCandidate) -> Self {
        Self {
            conversation: c.conversation,
            message_id: c.message_id,
            reply_to: c.reply_to,
            time_ms: c.time_ms,
            from_self: c.from_self,
            from_contact: c.from_contact,
            mentions_me: c.mentions_me,
            replies_to_me: c.replies_to_me,
            reactions_distinct: c.reactions_distinct,
            has_media: c.has_media,
            text: c.text,
        }
    }
}

/// One item to draw: a candidate and the replies folded under it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FeedEntry {
    /// Index into the candidates list.
    pub index: u32,
    /// Indices of its replies, oldest first.
    pub replies: Vec<u32>,
}

/// Encode a rule for storage, a `.lakkofeed` file or a link.
///
/// # Errors
///
/// [`FfiError::FeedRule`], the message starting with the frozen reason code
/// (`name`, `noKinds`, `words`, `tooLarge`, ...) followed by `": "`.
#[uniffi::export]
pub fn encode_feed_rule(rule: FeedRule) -> Result<Vec<u8>, FfiError> {
    rust_encode_feed_rule(&rule.into()).map_err(feed_rule_error)
}

/// Decode a rule someone else made. Refuses anything the encoder would not
/// produce.
///
/// # Errors
///
/// [`FfiError::FeedRule`] with a reason code first (`wrongDomain`,
/// `truncated`, `unknownFlag`, ...).
#[uniffi::export]
pub fn decode_feed_rule(bytes: Vec<u8>) -> Result<FeedRule, FfiError> {
    rust_decode_feed_rule(&bytes)
        .map(Into::into)
        .map_err(feed_rule_error)
}

/// Check a rule the person is editing, to explain a disabled Save.
///
/// # Errors
///
/// [`FfiError::FeedRule`] with the first failing bound's reason code.
#[uniffi::export]
pub fn validate_feed_rule(rule: FeedRule) -> Result<(), FfiError> {
    RustFeedRule::from(rule).validate().map_err(feed_rule_error)
}

/// The rule as it may leave this device: conversation ids removed.
#[uniffi::export]
#[must_use]
pub fn export_feed_rule(rule: FeedRule) -> FeedRule {
    RustFeedRule::from(rule).export().into()
}

/// The rule behind a preset.
#[uniffi::export]
#[must_use]
pub fn feed_preset_rule(preset: FeedPreset) -> FeedRule {
    rust_preset_rule(preset.into()).into()
}

/// Every preset, in the order the feed switcher shows them.
#[uniffi::export]
#[must_use]
pub fn feed_presets() -> Vec<FeedPreset> {
    vec![FeedPreset::Latest, FeedPreset::Popular, FeedPreset::ForMe]
}

/// Trim and lowercase a word the person typed; `None` when nothing is left.
#[uniffi::export]
#[must_use]
pub fn normalize_feed_word(raw: String) -> Option<String> {
    rust_normalize_word(&raw)
}

/// Rank the candidates under the rule at `now_ms`. Returns candidate
/// indices in display order, replies folded under their parent. Total: any
/// input returns an order.
#[uniffi::export]
#[must_use]
pub fn rank_feed(
    rule: FeedRule,
    conversations: Vec<FeedConversation>,
    candidates: Vec<FeedCandidate>,
    now_ms: i64,
) -> Vec<FeedEntry> {
    let rule: RustFeedRule = rule.into();
    let conversations: Vec<RustFeedConversation> =
        conversations.into_iter().map(Into::into).collect();
    let candidates: Vec<RustFeedCandidate> = candidates.into_iter().map(Into::into).collect();
    rust_rank(&rule, &conversations, &candidates, now_ms)
        .into_iter()
        .map(|e| FeedEntry {
            index: e.index,
            replies: e.replies,
        })
        .collect()
}

/// Whether `text` mentions `username` (`@` + the canonical username, whole).
#[uniffi::export]
#[must_use]
pub fn mentions_username(text: String, username: String) -> bool {
    rust_mentions_username(&text, &username)
}

/// How far a conversation is read: the highest id such that every id at or
/// below it is seen. Sent as the read receipt and the cross-device read
/// cursor instead of the highest received id.
#[uniffi::export]
#[must_use]
pub fn contiguous_seen(ids: Vec<i64>, seen: Vec<i64>) -> Option<i64> {
    rust_contiguous_seen(&ids, &seen)
}

/// Longest encoded rule, in bytes.
#[uniffi::export]
#[must_use]
pub fn feed_rule_max_len() -> u32 {
    u32::try_from(lakko_feed::FEED_RULE_MAX_LEN).unwrap_or(u32::MAX)
}

/// Most words in a word list.
#[uniffi::export]
#[must_use]
pub fn feed_max_words() -> u32 {
    u32::try_from(lakko_feed::MAX_WORDS).unwrap_or(u32::MAX)
}

/// Longest word, in UTF-8 bytes.
#[uniffi::export]
#[must_use]
pub fn feed_max_word_bytes() -> u32 {
    u32::try_from(lakko_feed::MAX_WORD_BYTES).unwrap_or(u32::MAX)
}

/// Highest weight.
#[uniffi::export]
#[must_use]
pub fn feed_max_weight() -> u8 {
    lakko_feed::MAX_WEIGHT
}

/// Longest window, in days.
#[uniffi::export]
#[must_use]
pub fn feed_max_window_days() -> u8 {
    lakko_feed::MAX_WINDOW_DAYS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_round_trips_through_the_ffi_types() {
        let mut rule = feed_preset_rule(FeedPreset::Popular);
        rule.include = vec![vec![1; 32]];
        rule.hide_words = vec!["spoiler".into()];
        let bytes = encode_feed_rule(rule.clone()).unwrap();
        assert_eq!(decode_feed_rule(bytes).unwrap(), rule);
        let exported = export_feed_rule(rule);
        assert!(exported.include.is_empty() && exported.sources_stripped);
    }

    #[test]
    fn errors_carry_the_reason_code_first() {
        let mut rule = feed_preset_rule(FeedPreset::Latest);
        rule.groups = false;
        rule.channels = false;
        match validate_feed_rule(rule) {
            Err(FfiError::FeedRule(msg)) => assert!(msg.starts_with("noKinds: "), "{msg}"),
            other => panic!("expected FeedRule, got {other:?}"),
        }
        match decode_feed_rule(b"nope".to_vec()) {
            Err(FfiError::FeedRule(msg)) => assert!(msg.starts_with("wrongDomain: "), "{msg}"),
            other => panic!("expected FeedRule, got {other:?}"),
        }
    }

    #[test]
    fn rank_passes_through() {
        let convs = vec![FeedConversation {
            id: vec![1],
            kind: FeedConversationKind::Group,
            muted: false,
            audience: 3,
        }];
        let cand = |id: i64, t: i64| FeedCandidate {
            conversation: 0,
            message_id: id,
            reply_to: None,
            time_ms: t,
            from_self: false,
            from_contact: false,
            mentions_me: false,
            replies_to_me: false,
            reactions_distinct: 0,
            has_media: false,
            text: String::new(),
        };
        let now = 1_800_000_000_000;
        let out = rank_feed(
            feed_preset_rule(FeedPreset::Latest),
            convs,
            vec![cand(1, now - 2000), cand(2, now - 1000)],
            now,
        );
        assert_eq!(out.iter().map(|e| e.index).collect::<Vec<_>>(), [1, 0]);
        assert_eq!(contiguous_seen(vec![1, 2, 3], vec![1, 3]), Some(1));
        assert!(mentions_username("hi @alice".into(), "alice".into()));
    }
}
