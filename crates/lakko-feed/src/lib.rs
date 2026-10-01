// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! The on-device feed (ADR-0061).
//!
//! Lakko's server reads no message, so a feed can only be built on the
//! device, from messages it already decrypted. This crate is the part of
//! that the three clients must not write three times: the rule a person
//! picks or writes, its bounded codec (a shared feed arrives as a file or a
//! link someone else made), the preset feeds, the ranking, the mention
//! match, and the definition of how far a conversation is read.
//!
//! It holds no messages, no keys and no state, and reaches for no screen,
//! store or network. The clients assemble candidates from their own logs and
//! draw the indices [`rank`] returns.
//!
//! Its own crate rather than a module of `lakko-ui-rules`, which declares
//! that nothing in it is security-relevant: the rule decoder here takes
//! bytes strangers send.

pub mod mention;
pub mod preset;
pub mod rank;
pub mod rule;
pub mod seen;

#[doc(hidden)]
pub mod fuzz_api;

pub use mention::mentions_username;
pub use preset::{preset_rule, Preset, ALL_PRESETS};
pub use rank::{
    rank, ConversationKind, FeedCandidate, FeedConversation, FeedEntry, MAX_REPLY_DEPTH, SCALE,
};
pub use rule::{
    decode_feed_rule, encode_feed_rule, normalize_word, FeedRule, FeedRuleError, HalfLife,
    Suggestions, Weights, FEED_RULE_DOMAIN_V1, FEED_RULE_MAX_LEN, FEED_RULE_VERSION_V1,
    MAX_CONVERSATIONS, MAX_CONVERSATION_ID_LEN, MAX_WEIGHT, MAX_WINDOW_DAYS, MAX_WORDS,
    MAX_WORD_BYTES, MIN_WINDOW_DAYS,
};
pub use seen::contiguous_seen;
