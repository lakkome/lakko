// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The on-device feed (ADR-0063) — wasm-bindgen surface.
//!
//! Mirror of `lakko-ffi-uniffi/src/feed.rs` over the same [`lakko_feed`]
//! core, so the web client draws the same order as the two native ones.
//!
//! Shapes follow the house style for small records crossing this boundary
//! (`ConversationRow`, `BeaconObservation`): constructible classes with
//! getters. A rule is a class with getters and setters, because the editor
//! changes one field at a time. Conversation-id lists cross as arrays of
//! `Uint8Array`. A refusal throws a real `Error` carrying the frozen
//! `FeedRuleError::code()` as an own `feedRuleError` property; the message
//! never contains the input.

use js_sys::{Array, Uint8Array};
use lakko_feed::{
    contiguous_seen as rust_contiguous_seen, decode_feed_rule as rust_decode_feed_rule,
    encode_feed_rule as rust_encode_feed_rule, mentions_username as rust_mentions_username,
    normalize_word as rust_normalize_word, preset_rule as rust_preset_rule, rank as rust_rank,
    ConversationKind as RustConversationKind, FeedCandidate as RustFeedCandidate,
    FeedConversation as RustFeedConversation, FeedRule as RustFeedRule,
    FeedRuleError as RustFeedRuleError, HalfLife as RustHalfLife, Preset as RustPreset,
    Suggestions as RustSuggestions,
};
use wasm_bindgen::prelude::*;

fn feed_rule_error(e: RustFeedRuleError) -> JsValue {
    let err = js_sys::Error::new(&e.to_string());
    // Best-effort, as in `channels.rs`: a failed property write must not
    // replace the error being reported.
    let _ = js_sys::Reflect::set(
        err.as_ref(),
        &JsValue::from_str("feedRuleError"),
        &JsValue::from_str(e.code()),
    );
    err.into()
}

fn ids_to_array(ids: &[Vec<u8>]) -> Array {
    ids.iter()
        .map(|id| JsValue::from(Uint8Array::from(id.as_slice())))
        .collect()
}

fn array_to_ids(arr: &Array) -> Result<Vec<Vec<u8>>, JsValue> {
    arr.iter()
        .map(|v| {
            v.dyn_into::<Uint8Array>().map(|u| u.to_vec()).map_err(|_| {
                JsValue::from(js_sys::Error::new("conversation ids must be Uint8Array"))
            })
        })
        .collect()
}

/// How fast an item's recency fades.
#[wasm_bindgen(js_name = FeedHalfLife)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedHalfLife {
    /// Six hours.
    FewHours = 0,
    /// One day.
    Day = 1,
    /// One week.
    Week = 2,
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
#[wasm_bindgen(js_name = FeedSuggestions)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedSuggestions {
    /// Never.
    Never = 0,
    /// Rarely.
    Rarely = 1,
    /// Sometimes.
    Sometimes = 2,
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

/// A built-in feed.
#[wasm_bindgen(js_name = FeedPreset)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedPreset {
    /// Every group and channel, newest first.
    Latest = 0,
    /// The last three days, group messages ranked by reactions and replies.
    Popular = 1,
    /// Mentions, replies to the person, their own reacted-to messages.
    ForMe = 2,
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

/// A feed rule. Field meanings and bounds are `lakko_feed::FeedRule`'s.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedRule {
    inner: RustFeedRule,
}

macro_rules! bool_field {
    ($get:ident, $set:ident, $js_get:literal, $js_set:literal, $field:ident, $doc:literal) => {
        #[wasm_bindgen]
        impl FeedRule {
            #[doc = $doc]
            #[wasm_bindgen(getter, js_name = $js_get)]
            #[must_use]
            pub fn $get(&self) -> bool {
                self.inner.$field
            }

            #[doc = $doc]
            #[wasm_bindgen(setter, js_name = $js_get)]
            pub fn $set(&mut self, value: bool) {
                self.inner.$field = value;
            }
        }
    };
}

macro_rules! weight_field {
    ($get:ident, $set:ident, $js:literal, $field:ident, $doc:literal) => {
        #[wasm_bindgen]
        impl FeedRule {
            #[doc = $doc]
            #[wasm_bindgen(getter, js_name = $js)]
            #[must_use]
            pub fn $get(&self) -> u8 {
                self.inner.weights.$field
            }

            #[doc = $doc]
            #[wasm_bindgen(setter, js_name = $js)]
            pub fn $set(&mut self, value: u8) {
                self.inner.weights.$field = value;
            }
        }
    };
}

#[wasm_bindgen]
impl FeedRule {
    /// A rule with this name that takes everything, newest first.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(name: String) -> Self {
        Self {
            inner: RustFeedRule::new(name),
        }
    }

    /// The rule behind a preset.
    #[must_use]
    pub fn preset(preset: FeedPreset) -> Self {
        Self {
            inner: rust_preset_rule(preset.into()),
        }
    }

    /// Decode a rule someone else made.
    ///
    /// # Errors
    ///
    /// Throws an `Error` with `feedRuleError` set to the reason code.
    pub fn decode(bytes: &[u8]) -> Result<Self, JsValue> {
        rust_decode_feed_rule(bytes)
            .map(|inner| Self { inner })
            .map_err(feed_rule_error)
    }

    /// Encode for storage, a `.lakkofeed` file or a link.
    ///
    /// # Errors
    ///
    /// Throws an `Error` with `feedRuleError` set to the reason code.
    pub fn encode(&self) -> Result<Vec<u8>, JsValue> {
        rust_encode_feed_rule(&self.inner).map_err(feed_rule_error)
    }

    /// Check every bound, to explain a disabled Save.
    ///
    /// # Errors
    ///
    /// Throws an `Error` with `feedRuleError` set to the reason code.
    pub fn validate(&self) -> Result<(), JsValue> {
        self.inner.validate().map_err(feed_rule_error)
    }

    /// The rule as it may leave this device: conversation ids removed.
    #[must_use]
    pub fn export(&self) -> Self {
        Self {
            inner: self.inner.export(),
        }
    }

    /// The feed's name.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn name(&self) -> String {
        self.inner.name.clone()
    }

    /// The feed's name.
    #[wasm_bindgen(setter)]
    pub fn set_name(&mut self, value: String) {
        self.inner.name = value;
    }

    /// Index into the client's fixed icon list.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn icon(&self) -> u8 {
        self.inner.icon
    }

    /// Index into the client's fixed icon list.
    #[wasm_bindgen(setter)]
    pub fn set_icon(&mut self, value: u8) {
        self.inner.icon = value;
    }

    /// How fast recency fades.
    #[wasm_bindgen(getter, js_name = halfLife)]
    #[must_use]
    pub fn half_life(&self) -> FeedHalfLife {
        self.inner.half_life.into()
    }

    /// How fast recency fades.
    #[wasm_bindgen(setter, js_name = halfLife)]
    pub fn set_half_life(&mut self, value: FeedHalfLife) {
        self.inner.half_life = value.into();
    }

    /// How far back the feed looks, 1..=30 days.
    #[wasm_bindgen(getter, js_name = windowDays)]
    #[must_use]
    pub fn window_days(&self) -> u8 {
        self.inner.window_days
    }

    /// How far back the feed looks, 1..=30 days.
    #[wasm_bindgen(setter, js_name = windowDays)]
    pub fn set_window_days(&mut self, value: u8) {
        self.inner.window_days = value;
    }

    /// Only messages with at least this many people reacting.
    #[wasm_bindgen(getter, js_name = minReactions)]
    #[must_use]
    pub fn min_reactions(&self) -> u16 {
        self.inner.min_reactions
    }

    /// Only messages with at least this many people reacting.
    #[wasm_bindgen(setter, js_name = minReactions)]
    pub fn set_min_reactions(&mut self, value: u16) {
        self.inner.min_reactions = value;
    }

    /// How often suggestions appear.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn suggestions(&self) -> FeedSuggestions {
        self.inner.suggestions.into()
    }

    /// How often suggestions appear.
    #[wasm_bindgen(setter)]
    pub fn set_suggestions(&mut self, value: FeedSuggestions) {
        self.inner.suggestions = value.into();
    }

    /// Drop a message containing any of these (lowercase, whole words).
    #[wasm_bindgen(getter, js_name = hideWords)]
    #[must_use]
    pub fn hide_words(&self) -> Vec<String> {
        self.inner.hide_words.clone()
    }

    /// Drop a message containing any of these (lowercase, whole words).
    #[wasm_bindgen(setter, js_name = hideWords)]
    pub fn set_hide_words(&mut self, value: Vec<String>) {
        self.inner.hide_words = value;
    }

    /// When non-empty, keep only messages containing one of these.
    #[wasm_bindgen(getter, js_name = onlyWords)]
    #[must_use]
    pub fn only_words(&self) -> Vec<String> {
        self.inner.only_words.clone()
    }

    /// When non-empty, keep only messages containing one of these.
    #[wasm_bindgen(setter, js_name = onlyWords)]
    pub fn set_only_words(&mut self, value: Vec<String>) {
        self.inner.only_words = value;
    }

    /// Only these conversations, when non-empty. Never exported.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn include(&self) -> Array {
        ids_to_array(&self.inner.include)
    }

    /// Only these conversations, when non-empty.
    ///
    /// # Errors
    ///
    /// Throws when an element is not a `Uint8Array`.
    #[wasm_bindgen(js_name = setInclude)]
    pub fn set_include(&mut self, ids: &Array) -> Result<(), JsValue> {
        self.inner.include = array_to_ids(ids)?;
        Ok(())
    }

    /// Never these conversations. Never exported.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn exclude(&self) -> Array {
        ids_to_array(&self.inner.exclude)
    }

    /// Never these conversations.
    ///
    /// # Errors
    ///
    /// Throws when an element is not a `Uint8Array`.
    #[wasm_bindgen(js_name = setExclude)]
    pub fn set_exclude(&mut self, ids: &Array) -> Result<(), JsValue> {
        self.inner.exclude = array_to_ids(ids)?;
        Ok(())
    }
}

bool_field!(
    groups,
    set_groups,
    "groups",
    "groups",
    groups,
    "Take group messages."
);
bool_field!(
    channels,
    set_channels,
    "channels",
    "channels",
    channels,
    "Take channel posts."
);
bool_field!(
    sources_stripped,
    set_sources_stripped,
    "sourcesStripped",
    "sourcesStripped",
    sources_stripped,
    "Set by export when it removed include / exclude."
);
bool_field!(
    include_muted,
    set_include_muted,
    "includeMuted",
    "includeMuted",
    include_muted,
    "Take muted conversations too."
);
bool_field!(
    media_only,
    set_media_only,
    "mediaOnly",
    "mediaOnly",
    media_only,
    "Only messages with an attachment."
);
bool_field!(
    hide_own,
    set_hide_own,
    "hideOwn",
    "hideOwn",
    hide_own,
    "Never the person's own messages."
);
bool_field!(
    for_me,
    set_for_me,
    "forMe",
    "forMe",
    for_me,
    "Only mentions, replies to the person, and their own reacted-to messages."
);

weight_field!(
    weight_recency,
    set_weight_recency,
    "weightRecency",
    recency,
    "Weight of recency, 0..=100."
);
weight_field!(
    weight_reactions,
    set_weight_reactions,
    "weightReactions",
    reactions,
    "Weight of reactions, 0..=100."
);
weight_field!(
    weight_replies,
    set_weight_replies,
    "weightReplies",
    replies,
    "Weight of replies, 0..=100."
);
weight_field!(
    weight_from_contacts,
    set_weight_from_contacts,
    "weightFromContacts",
    from_contacts,
    "Weight of messages from contacts, 0..=100."
);
weight_field!(
    weight_mentions_me,
    set_weight_mentions_me,
    "weightMentionsMe",
    mentions_me,
    "Weight of mentions and replies to the person, 0..=100."
);
weight_field!(
    weight_has_media,
    set_weight_has_media,
    "weightHasMedia",
    has_media,
    "Weight of attachments, 0..=100."
);

/// Group or channel. One-to-one chats have no value here.
#[wasm_bindgen(js_name = FeedConversationKind)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedConversationKind {
    /// A group.
    Group = 0,
    /// A channel.
    Channel = 1,
}

/// One conversation the client offers messages from.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedConversation {
    inner: RustFeedConversation,
}

#[wasm_bindgen]
impl FeedConversation {
    /// `id` is compared with the rule's include / exclude lists; `audience`
    /// is the member count (ignored for a channel).
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(id: Vec<u8>, kind: FeedConversationKind, muted: bool, audience: u32) -> Self {
        let kind = match kind {
            FeedConversationKind::Group => RustConversationKind::Group,
            FeedConversationKind::Channel => RustConversationKind::Channel,
        };
        Self {
            inner: RustFeedConversation {
                id,
                kind,
                muted,
                audience,
            },
        }
    }
}

/// One message, reduced to what ranking needs.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedCandidate {
    inner: RustFeedCandidate,
}

#[wasm_bindgen]
impl FeedCandidate {
    /// Build a candidate. `messageId`, `replyTo` and `timeMs` are BigInt;
    /// `replyTo` is `undefined` when it replies to nothing. `text` is used
    /// only by the word filters and never logged.
    #[wasm_bindgen(constructor)]
    #[must_use]
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    pub fn new(
        conversation: u32,
        message_id: i64,
        reply_to: Option<i64>,
        time_ms: i64,
        from_self: bool,
        from_contact: bool,
        mentions_me: bool,
        replies_to_me: bool,
        reactions_distinct: u32,
        has_media: bool,
        text: String,
    ) -> Self {
        Self {
            inner: RustFeedCandidate {
                conversation,
                message_id,
                reply_to,
                time_ms,
                from_self,
                from_contact,
                mentions_me,
                replies_to_me,
                reactions_distinct,
                has_media,
                text,
            },
        }
    }
}

/// One item to draw: a candidate and the replies folded under it.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedEntry {
    index: u32,
    replies: Vec<u32>,
}

#[wasm_bindgen]
impl FeedEntry {
    /// Index into the candidates array.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn index(&self) -> u32 {
        self.index
    }

    /// Indices of its replies, oldest first.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn replies(&self) -> Vec<u32> {
        self.replies.clone()
    }
}

/// Rank the candidates under the rule at `nowMs` (BigInt). Returns entries
/// in display order, replies folded under their parent.
#[wasm_bindgen(js_name = rankFeed)]
#[must_use]
pub fn rank_feed(
    rule: &FeedRule,
    conversations: Vec<FeedConversation>,
    candidates: Vec<FeedCandidate>,
    now_ms: i64,
) -> Vec<FeedEntry> {
    let conversations: Vec<RustFeedConversation> =
        conversations.into_iter().map(|c| c.inner).collect();
    let candidates: Vec<RustFeedCandidate> = candidates.into_iter().map(|c| c.inner).collect();
    rust_rank(&rule.inner, &conversations, &candidates, now_ms)
        .into_iter()
        .map(|e| FeedEntry {
            index: e.index,
            replies: e.replies,
        })
        .collect()
}

/// Trim and lowercase a word the person typed; `undefined` when nothing is
/// left.
#[wasm_bindgen(js_name = normalizeFeedWord)]
#[must_use]
pub fn normalize_feed_word(raw: &str) -> Option<String> {
    rust_normalize_word(raw)
}

/// Whether `text` mentions `username` (`@` + the canonical username, whole).
#[wasm_bindgen(js_name = mentionsUsername)]
#[must_use]
pub fn mentions_username(text: &str, username: &str) -> bool {
    rust_mentions_username(text, username)
}

/// How far a conversation is read: the highest id such that every id at or
/// below it is seen, or `undefined`. Ids are BigInt arrays.
#[wasm_bindgen(js_name = contiguousSeen)]
#[must_use]
pub fn contiguous_seen(ids: Vec<i64>, seen: Vec<i64>) -> Option<i64> {
    rust_contiguous_seen(&ids, &seen)
}

/// Longest encoded rule, in bytes.
#[wasm_bindgen(js_name = feedRuleMaxLen)]
#[must_use]
pub fn feed_rule_max_len() -> u32 {
    u32::try_from(lakko_feed::FEED_RULE_MAX_LEN).unwrap_or(u32::MAX)
}

/// Most words in a word list.
#[wasm_bindgen(js_name = feedMaxWords)]
#[must_use]
pub fn feed_max_words() -> u32 {
    u32::try_from(lakko_feed::MAX_WORDS).unwrap_or(u32::MAX)
}

/// Longest word, in UTF-8 bytes.
#[wasm_bindgen(js_name = feedMaxWordBytes)]
#[must_use]
pub fn feed_max_word_bytes() -> u32 {
    u32::try_from(lakko_feed::MAX_WORD_BYTES).unwrap_or(u32::MAX)
}

/// Highest weight.
#[wasm_bindgen(js_name = feedMaxWeight)]
#[must_use]
pub fn feed_max_weight() -> u8 {
    lakko_feed::MAX_WEIGHT
}

/// Longest window, in days.
#[wasm_bindgen(js_name = feedMaxWindowDays)]
#[must_use]
pub fn feed_max_window_days() -> u8 {
    lakko_feed::MAX_WINDOW_DAYS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_encodes_and_decodes_to_the_same_rule() {
        let rule = FeedRule::preset(FeedPreset::Popular);
        let bytes = rust_encode_feed_rule(&rule.inner).unwrap();
        assert_eq!(rust_decode_feed_rule(&bytes).unwrap(), rule.inner);
    }

    #[test]
    fn rank_passes_through() {
        let now = 1_800_000_000_000;
        let convs = vec![FeedConversation::new(
            vec![1],
            FeedConversationKind::Group,
            false,
            3,
        )];
        let c = |id: i64, t: i64| {
            FeedCandidate::new(
                0,
                id,
                None,
                t,
                false,
                false,
                false,
                false,
                0,
                false,
                String::new(),
            )
        };
        let out = rank_feed(
            &FeedRule::preset(FeedPreset::Latest),
            convs,
            vec![c(1, now - 2000), c(2, now - 1000)],
            now,
        );
        assert_eq!(out.iter().map(FeedEntry::index).collect::<Vec<_>>(), [1, 0]);
        assert_eq!(contiguous_seen(vec![1, 2, 3], vec![1, 3]), Some(1));
    }
}
