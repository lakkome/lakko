// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A feed rule and its wire codec (ADR-0058 section 1).
//!
//! ## Wire
//!
//! ```text
//! DOMAIN "lakko/feed-rule/v1\0" (19)
//! version        u8   = 0x01
//! flags          u8   bit0 groups, bit1 channels, bit2 include_muted,
//!                     bit3 media_only, bit4 hide_own, bit5 for_me,
//!                     bit6 sources_stripped, bit7 must be 0
//! icon           u8
//! half_life      u8   0 few hours, 1 day, 2 week
//! suggestions    u8   0 never, 1 rarely, 2 sometimes
//! window_days    u8   1..=30
//! weights        6 x u8, each 0..=100: recency, reactions, replies,
//!                     from_contacts, mentions_me, has_media
//! min_reactions  u16 BE
//! name_len u8 ‖ name
//! include_count u16 BE ‖ [id_len u8 ‖ id]
//! exclude_count u16 BE ‖ [id_len u8 ‖ id]
//! hide_count u8 ‖ [word_len u8 ‖ word]
//! only_count u8 ‖ [word_len u8 ‖ word]
//! ```
//!
//! The whole encoding is at most [`FEED_RULE_MAX_LEN`] bytes, so an
//! exported rule fits in a link. Decoding refuses trailing bytes, unknown
//! flag bits and every bound [`FeedRule::validate`] checks, so the encoding
//! is canonical: whatever decodes re-encodes to the identical bytes.
//!
//! ## Hostile input
//!
//! A shared feed arrives as a file or a link someone else made. The decoder
//! is therefore written for attacker-chosen bytes: every length is checked
//! against the remaining input before it is used, counts are bounded before
//! anything is allocated, and nothing panics (fuzz target
//! `feed_rule_decode`). Errors are reason codes and never echo the input.
//!
//! ## Conversation ids are local
//!
//! `include` / `exclude` name this device's conversations. They mean nothing
//! to another person and would tell them which conversations the author is
//! in, so [`FeedRule::export`] empties them and sets `sources_stripped`,
//! which the importing client shows as "Sources: all groups and channels".

use lakko_contacts::{check_display_name, DisplayNameError};
use thiserror::Error;

/// Domain prefix of an encoded rule. Frozen.
pub const FEED_RULE_DOMAIN_V1: &[u8] = b"lakko/feed-rule/v1\0";

/// The one version this codec reads and writes.
pub const FEED_RULE_VERSION_V1: u8 = 0x01;

/// Longest encoded rule, in bytes.
pub const FEED_RULE_MAX_LEN: usize = 4096;

/// Most conversation ids in `include` or in `exclude`.
pub const MAX_CONVERSATIONS: usize = 256;

/// Longest conversation id, in bytes.
pub const MAX_CONVERSATION_ID_LEN: usize = 64;

/// Most words in `hide_words` or in `only_words`.
pub const MAX_WORDS: usize = 32;

/// Longest word or phrase, in UTF-8 bytes.
pub const MAX_WORD_BYTES: usize = 64;

/// Highest weight a signal can carry.
pub const MAX_WEIGHT: u8 = 100;

/// Shortest window, in days.
pub const MIN_WINDOW_DAYS: u8 = 1;

/// Longest window, in days. Matches the 30 days the server keeps a message,
/// beyond which no device can have received something it does not already
/// hold.
pub const MAX_WINDOW_DAYS: u8 = 30;

const FLAG_GROUPS: u8 = 1 << 0;
const FLAG_CHANNELS: u8 = 1 << 1;
const FLAG_INCLUDE_MUTED: u8 = 1 << 2;
const FLAG_MEDIA_ONLY: u8 = 1 << 3;
const FLAG_HIDE_OWN: u8 = 1 << 4;
const FLAG_FOR_ME: u8 = 1 << 5;
const FLAG_SOURCES_STRIPPED: u8 = 1 << 6;
const FLAG_KNOWN: u8 = FLAG_GROUPS
    | FLAG_CHANNELS
    | FLAG_INCLUDE_MUTED
    | FLAG_MEDIA_ONLY
    | FLAG_HIDE_OWN
    | FLAG_FOR_ME
    | FLAG_SOURCES_STRIPPED;

/// How fast an item's recency fades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HalfLife {
    /// Six hours.
    FewHours,
    /// One day.
    Day,
    /// One week.
    Week,
}

impl HalfLife {
    /// The half-life in milliseconds.
    #[must_use]
    pub const fn millis(self) -> i64 {
        match self {
            Self::FewHours => 6 * 3_600_000,
            Self::Day => 86_400_000,
            Self::Week => 7 * 86_400_000,
        }
    }

    const fn to_byte(self) -> u8 {
        match self {
            Self::FewHours => 0,
            Self::Day => 1,
            Self::Week => 2,
        }
    }

    const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::FewHours),
            1 => Some(Self::Day),
            2 => Some(Self::Week),
            _ => None,
        }
    }
}

/// How often a suggested channel appears in this feed (ADR-0059 reads it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Suggestions {
    /// Never.
    Never,
    /// Rarely.
    Rarely,
    /// Sometimes.
    Sometimes,
}

impl Suggestions {
    const fn to_byte(self) -> u8 {
        match self {
            Self::Never => 0,
            Self::Rarely => 1,
            Self::Sometimes => 2,
        }
    }

    const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Never),
            1 => Some(Self::Rarely),
            2 => Some(Self::Sometimes),
            _ => None,
        }
    }
}

/// How much each signal counts, each `0..=MAX_WEIGHT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Weights {
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
    /// Carries a photo, video or other attachment.
    pub has_media: u8,
}

impl Weights {
    const fn as_array(self) -> [u8; 6] {
        [
            self.recency,
            self.reactions,
            self.replies,
            self.from_contacts,
            self.mentions_me,
            self.has_media,
        ]
    }
}

/// A feed: which messages it takes, what counts, and what it filters out.
// The bools are independent switches the person sets and the wire carries as
// flag bits; there is no enum they could collapse into.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedRule {
    /// The feed's name, under the display-name rule of ADR-0048.
    pub name: String,
    /// Index into the clients' fixed icon list; an unknown index draws the
    /// default icon, so any value is accepted.
    pub icon: u8,
    /// Take group messages.
    pub groups: bool,
    /// Take channel posts.
    pub channels: bool,
    /// Only these conversations, when non-empty. Local ids, never exported.
    pub include: Vec<Vec<u8>>,
    /// Never these conversations. Local ids, never exported.
    pub exclude: Vec<Vec<u8>>,
    /// Set by [`FeedRule::export`] when it removed `include` / `exclude`.
    pub sources_stripped: bool,
    /// Take muted conversations too.
    pub include_muted: bool,
    /// What counts, and how much.
    pub weights: Weights,
    /// How fast recency fades.
    pub half_life: HalfLife,
    /// How far back the feed looks, in days.
    pub window_days: u8,
    /// Only messages with an attachment.
    pub media_only: bool,
    /// Never the person's own messages.
    pub hide_own: bool,
    /// Only messages that mention the person, reply to them, or are the
    /// person's own and drew a reaction.
    pub for_me: bool,
    /// Only messages with at least this many people reacting.
    pub min_reactions: u16,
    /// Drop a message containing any of these (lowercase, whole words).
    pub hide_words: Vec<String>,
    /// When non-empty, keep only messages containing one of these.
    pub only_words: Vec<String>,
    /// How often suggestions appear (ADR-0059).
    pub suggestions: Suggestions,
}

/// Why a rule was refused. Reason codes only: no variant carries input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FeedRuleError {
    /// The bytes do not start with [`FEED_RULE_DOMAIN_V1`].
    #[error("not a feed rule")]
    WrongDomain,
    /// A version this codec does not read.
    #[error("unknown feed rule version")]
    UnknownVersion,
    /// The input ended before a field did.
    #[error("feed rule is truncated")]
    Truncated,
    /// Bytes remain after the last field.
    #[error("feed rule has trailing bytes")]
    TrailingBytes,
    /// Longer than [`FEED_RULE_MAX_LEN`].
    #[error("feed rule is too large")]
    TooLarge,
    /// A flag bit outside the defined set is set.
    #[error("feed rule has an unknown flag")]
    UnknownFlag,
    /// An enum byte outside its range.
    #[error("feed rule has an unknown option")]
    UnknownOption,
    /// The name fails the display-name rule.
    #[error("feed name is not admissible: {0}")]
    Name(DisplayNameError),
    /// Neither groups nor channels.
    #[error("feed takes no kind of conversation")]
    NoKinds,
    /// A weight above [`MAX_WEIGHT`].
    #[error("a weight is out of range")]
    WeightOutOfRange,
    /// A window outside [`MIN_WINDOW_DAYS`]..=[`MAX_WINDOW_DAYS`].
    #[error("the window is out of range")]
    WindowOutOfRange,
    /// Too many, empty, too long, duplicated or overlapping conversation ids.
    #[error("the conversation list is not admissible")]
    Conversations,
    /// `sources_stripped` is set while conversations are listed.
    #[error("a stripped rule lists conversations")]
    StrippedWithSources,
    /// Too many, empty, untrimmed, uppercase, control-bearing, too long or
    /// duplicated words.
    #[error("the word list is not admissible")]
    Words,
}

impl FeedRuleError {
    /// Stable, language-neutral reason code for the FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion => "unknownVersion",
            Self::Truncated => "truncated",
            Self::TrailingBytes => "trailingBytes",
            Self::TooLarge => "tooLarge",
            Self::UnknownFlag => "unknownFlag",
            Self::UnknownOption => "unknownOption",
            Self::Name(_) => "name",
            Self::NoKinds => "noKinds",
            Self::WeightOutOfRange => "weightOutOfRange",
            Self::WindowOutOfRange => "windowOutOfRange",
            Self::Conversations => "conversations",
            Self::StrippedWithSources => "strippedWithSources",
            Self::Words => "words",
        }
    }
}

impl FeedRule {
    /// A rule with the given name that takes everything, newest first.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            icon: 0,
            groups: true,
            channels: true,
            include: Vec::new(),
            exclude: Vec::new(),
            sources_stripped: false,
            include_muted: false,
            weights: Weights {
                recency: MAX_WEIGHT,
                ..Weights::default()
            },
            half_life: HalfLife::Day,
            window_days: MAX_WINDOW_DAYS,
            media_only: false,
            hide_own: false,
            for_me: false,
            min_reactions: 0,
            hide_words: Vec::new(),
            only_words: Vec::new(),
            suggestions: Suggestions::Never,
        }
    }

    /// Check every bound except the encoded size. The single rule both
    /// [`encode_feed_rule`] and [`decode_feed_rule`] run.
    pub fn validate(&self) -> Result<(), FeedRuleError> {
        check_display_name(&self.name).map_err(FeedRuleError::Name)?;
        if !self.groups && !self.channels {
            return Err(FeedRuleError::NoKinds);
        }
        if self.weights.as_array().iter().any(|w| *w > MAX_WEIGHT) {
            return Err(FeedRuleError::WeightOutOfRange);
        }
        if !(MIN_WINDOW_DAYS..=MAX_WINDOW_DAYS).contains(&self.window_days) {
            return Err(FeedRuleError::WindowOutOfRange);
        }
        check_conversations(&self.include)?;
        check_conversations(&self.exclude)?;
        if self.include.iter().any(|id| self.exclude.contains(id)) {
            return Err(FeedRuleError::Conversations);
        }
        if self.sources_stripped && (!self.include.is_empty() || !self.exclude.is_empty()) {
            return Err(FeedRuleError::StrippedWithSources);
        }
        check_words(&self.hide_words)?;
        check_words(&self.only_words)?;
        Ok(())
    }

    /// The rule as it may leave this device: conversation ids removed, and
    /// `sources_stripped` set when there were any.
    #[must_use]
    pub fn export(&self) -> Self {
        let mut out = self.clone();
        if !out.include.is_empty() || !out.exclude.is_empty() {
            out.include.clear();
            out.exclude.clear();
            out.sources_stripped = true;
        }
        out
    }

    const fn flags(&self) -> u8 {
        let mut f = 0;
        if self.groups {
            f |= FLAG_GROUPS;
        }
        if self.channels {
            f |= FLAG_CHANNELS;
        }
        if self.include_muted {
            f |= FLAG_INCLUDE_MUTED;
        }
        if self.media_only {
            f |= FLAG_MEDIA_ONLY;
        }
        if self.hide_own {
            f |= FLAG_HIDE_OWN;
        }
        if self.for_me {
            f |= FLAG_FOR_ME;
        }
        if self.sources_stripped {
            f |= FLAG_SOURCES_STRIPPED;
        }
        f
    }
}

fn check_conversations(ids: &[Vec<u8>]) -> Result<(), FeedRuleError> {
    if ids.len() > MAX_CONVERSATIONS {
        return Err(FeedRuleError::Conversations);
    }
    for (i, id) in ids.iter().enumerate() {
        if id.is_empty() || id.len() > MAX_CONVERSATION_ID_LEN || ids[..i].contains(id) {
            return Err(FeedRuleError::Conversations);
        }
    }
    Ok(())
}

/// Normalise a word the person typed into the form a rule stores: trimmed
/// and lowercase. Returns `None` when nothing is left.
#[must_use]
pub fn normalize_word(raw: &str) -> Option<String> {
    let w = raw.trim().to_lowercase();
    if w.is_empty() {
        None
    } else {
        Some(w)
    }
}

fn check_words(words: &[String]) -> Result<(), FeedRuleError> {
    if words.len() > MAX_WORDS {
        return Err(FeedRuleError::Words);
    }
    for (i, w) in words.iter().enumerate() {
        let admissible = !w.is_empty()
            && w.len() <= MAX_WORD_BYTES
            && w.trim() == w
            && w.to_lowercase() == *w
            && !w.chars().any(char::is_control)
            && !words[..i].contains(w);
        if !admissible {
            return Err(FeedRuleError::Words);
        }
    }
    Ok(())
}

/// Encode a rule.
pub fn encode_feed_rule(rule: &FeedRule) -> Result<Vec<u8>, FeedRuleError> {
    rule.validate()?;
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(FEED_RULE_DOMAIN_V1);
    out.push(FEED_RULE_VERSION_V1);
    out.push(rule.flags());
    out.push(rule.icon);
    out.push(rule.half_life.to_byte());
    out.push(rule.suggestions.to_byte());
    out.push(rule.window_days);
    out.extend_from_slice(&rule.weights.as_array());
    out.extend_from_slice(&rule.min_reactions.to_be_bytes());
    push_u8_len(&mut out, rule.name.as_bytes())?;
    for list in [&rule.include, &rule.exclude] {
        let count = u16::try_from(list.len()).map_err(|_| FeedRuleError::Conversations)?;
        out.extend_from_slice(&count.to_be_bytes());
        for id in list {
            push_u8_len(&mut out, id)?;
        }
    }
    for list in [&rule.hide_words, &rule.only_words] {
        let count = u8::try_from(list.len()).map_err(|_| FeedRuleError::Words)?;
        out.push(count);
        for w in list {
            push_u8_len(&mut out, w.as_bytes())?;
        }
    }
    if out.len() > FEED_RULE_MAX_LEN {
        return Err(FeedRuleError::TooLarge);
    }
    Ok(out)
}

fn push_u8_len(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), FeedRuleError> {
    let len = u8::try_from(bytes.len()).map_err(|_| FeedRuleError::TooLarge)?;
    out.push(len);
    out.extend_from_slice(bytes);
    Ok(())
}

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], FeedRuleError> {
        if self.rest.len() < n {
            return Err(FeedRuleError::Truncated);
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, FeedRuleError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, FeedRuleError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn short_bytes(&mut self) -> Result<&'a [u8], FeedRuleError> {
        let len = usize::from(self.u8()?);
        self.take(len)
    }

    fn short_str(&mut self, err: FeedRuleError) -> Result<String, FeedRuleError> {
        let b = self.short_bytes()?;
        std::str::from_utf8(b).map(str::to_owned).map_err(|_| err)
    }
}

/// Decode a rule, refusing anything [`encode_feed_rule`] would not produce.
pub fn decode_feed_rule(bytes: &[u8]) -> Result<FeedRule, FeedRuleError> {
    if bytes.len() > FEED_RULE_MAX_LEN {
        return Err(FeedRuleError::TooLarge);
    }
    let Some(body) = bytes.strip_prefix(FEED_RULE_DOMAIN_V1) else {
        return Err(FeedRuleError::WrongDomain);
    };
    let mut r = Reader { rest: body };
    if r.u8()? != FEED_RULE_VERSION_V1 {
        return Err(FeedRuleError::UnknownVersion);
    }
    let flags = r.u8()?;
    if flags & !FLAG_KNOWN != 0 {
        return Err(FeedRuleError::UnknownFlag);
    }
    let icon = r.u8()?;
    let half_life = HalfLife::from_byte(r.u8()?).ok_or(FeedRuleError::UnknownOption)?;
    let suggestions = Suggestions::from_byte(r.u8()?).ok_or(FeedRuleError::UnknownOption)?;
    let window_days = r.u8()?;
    let w = r.take(6)?;
    let weights = Weights {
        recency: w[0],
        reactions: w[1],
        replies: w[2],
        from_contacts: w[3],
        mentions_me: w[4],
        has_media: w[5],
    };
    let min_reactions = r.u16()?;
    let name = r.short_str(FeedRuleError::Name(DisplayNameError::NotUtf8))?;

    let mut lists: [Vec<Vec<u8>>; 2] = [Vec::new(), Vec::new()];
    for list in &mut lists {
        let count = usize::from(r.u16()?);
        if count > MAX_CONVERSATIONS {
            return Err(FeedRuleError::Conversations);
        }
        for _ in 0..count {
            list.push(r.short_bytes()?.to_vec());
        }
    }
    let [include, exclude] = lists;

    let mut words: [Vec<String>; 2] = [Vec::new(), Vec::new()];
    for list in &mut words {
        let count = usize::from(r.u8()?);
        if count > MAX_WORDS {
            return Err(FeedRuleError::Words);
        }
        for _ in 0..count {
            list.push(r.short_str(FeedRuleError::Words)?);
        }
    }
    let [hide_words, only_words] = words;

    if !r.rest.is_empty() {
        return Err(FeedRuleError::TrailingBytes);
    }

    let rule = FeedRule {
        name,
        icon,
        groups: flags & FLAG_GROUPS != 0,
        channels: flags & FLAG_CHANNELS != 0,
        include,
        exclude,
        sources_stripped: flags & FLAG_SOURCES_STRIPPED != 0,
        include_muted: flags & FLAG_INCLUDE_MUTED != 0,
        weights,
        half_life,
        window_days,
        media_only: flags & FLAG_MEDIA_ONLY != 0,
        hide_own: flags & FLAG_HIDE_OWN != 0,
        for_me: flags & FLAG_FOR_ME != 0,
        min_reactions,
        hide_words,
        only_words,
        suggestions,
    };
    rule.validate()?;
    Ok(rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> FeedRule {
        FeedRule {
            name: "Book club".into(),
            icon: 3,
            groups: true,
            channels: false,
            include: vec![vec![1; 32], vec![2; 16]],
            exclude: vec![vec![3; 32]],
            sources_stripped: false,
            include_muted: true,
            weights: Weights {
                recency: 40,
                reactions: 100,
                replies: 60,
                from_contacts: 10,
                mentions_me: 5,
                has_media: 0,
            },
            half_life: HalfLife::Week,
            window_days: 7,
            media_only: false,
            hide_own: true,
            for_me: false,
            min_reactions: 2,
            hide_words: vec!["spoiler".into(), "два слова".into()],
            only_words: vec!["books".into()],
            suggestions: Suggestions::Rarely,
        }
    }

    #[test]
    fn round_trip_is_identical() {
        let rule = full();
        let bytes = encode_feed_rule(&rule).unwrap();
        assert_eq!(decode_feed_rule(&bytes).unwrap(), rule);
        assert_eq!(
            encode_feed_rule(&decode_feed_rule(&bytes).unwrap()).unwrap(),
            bytes
        );
    }

    #[test]
    fn domain_is_frozen() {
        assert_eq!(FEED_RULE_DOMAIN_V1, b"lakko/feed-rule/v1\0");
        let bytes = encode_feed_rule(&FeedRule::new("All")).unwrap();
        assert_eq!(&bytes[..19], FEED_RULE_DOMAIN_V1);
        assert_eq!(bytes[19], FEED_RULE_VERSION_V1);
    }

    #[test]
    fn export_strips_conversations_and_says_so() {
        let exported = full().export();
        assert!(exported.include.is_empty() && exported.exclude.is_empty());
        assert!(exported.sources_stripped);
        let bytes = encode_feed_rule(&exported).unwrap();
        let back = decode_feed_rule(&bytes).unwrap();
        assert!(back.sources_stripped);
        // A rule that named no conversation is not marked stripped.
        assert!(!FeedRule::new("All").export().sources_stripped);
    }

    #[test]
    fn stripped_rule_with_sources_is_refused() {
        let mut r = full();
        r.sources_stripped = true;
        assert_eq!(
            encode_feed_rule(&r),
            Err(FeedRuleError::StrippedWithSources)
        );
    }

    #[test]
    fn bounds_are_enforced() {
        let mut r = FeedRule::new("x");
        r.groups = false;
        r.channels = false;
        assert_eq!(r.validate(), Err(FeedRuleError::NoKinds));

        let mut r = FeedRule::new("x");
        r.weights.replies = 101;
        assert_eq!(r.validate(), Err(FeedRuleError::WeightOutOfRange));

        for days in [0, 31] {
            let mut r = FeedRule::new("x");
            r.window_days = days;
            assert_eq!(r.validate(), Err(FeedRuleError::WindowOutOfRange));
        }

        let mut r = FeedRule::new("x");
        r.include = vec![vec![1], vec![1]];
        assert_eq!(r.validate(), Err(FeedRuleError::Conversations));

        let mut r = FeedRule::new("x");
        r.include = vec![vec![1]];
        r.exclude = vec![vec![1]];
        assert_eq!(r.validate(), Err(FeedRuleError::Conversations));

        for bad in ["Upper", " padded", "", "line\nbreak", "dup"] {
            let mut r = FeedRule::new("x");
            r.hide_words = if bad == "dup" {
                vec!["a".into(), "a".into()]
            } else {
                vec![bad.into()]
            };
            assert_eq!(r.validate(), Err(FeedRuleError::Words), "{bad:?}");
        }

        let mut r = FeedRule::new(" padded ");
        r.groups = true;
        assert!(matches!(r.validate(), Err(FeedRuleError::Name(_))));
    }

    #[test]
    fn oversized_rule_is_refused_on_encode() {
        let mut r = FeedRule::new("x");
        r.include = (0..MAX_CONVERSATIONS)
            .map(|i| {
                let mut id = vec![0u8; MAX_CONVERSATION_ID_LEN];
                id[..8].copy_from_slice(&(i as u64).to_be_bytes());
                id
            })
            .collect();
        assert_eq!(encode_feed_rule(&r), Err(FeedRuleError::TooLarge));
    }

    #[test]
    fn decoder_refuses_what_encoder_never_writes() {
        let good = encode_feed_rule(&full()).unwrap();

        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            decode_feed_rule(&trailing),
            Err(FeedRuleError::TrailingBytes)
        );

        let mut flag = good.clone();
        flag[20] |= 0x80;
        assert_eq!(decode_feed_rule(&flag), Err(FeedRuleError::UnknownFlag));

        let mut version = good.clone();
        version[19] = 2;
        assert_eq!(
            decode_feed_rule(&version),
            Err(FeedRuleError::UnknownVersion)
        );

        let mut half = good.clone();
        half[22] = 9;
        assert_eq!(decode_feed_rule(&half), Err(FeedRuleError::UnknownOption));

        for cut in 0..good.len() {
            assert!(decode_feed_rule(&good[..cut]).is_err(), "cut at {cut}");
        }
        assert_eq!(decode_feed_rule(b"nope"), Err(FeedRuleError::WrongDomain));
    }

    #[test]
    fn normalize_word_trims_and_lowercases() {
        assert_eq!(normalize_word("  Spoiler "), Some("spoiler".into()));
        assert_eq!(normalize_word("   "), None);
    }
}
