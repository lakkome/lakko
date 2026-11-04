// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the emoji and reaction presentation rules
//! (`lakko_ui_rules::{emoji, reaction}`, UX `docs/ux/reactions-and-emoji.md`
//! sections 17 and 18): which text is one emoji, which messages draw large,
//! the fold of reaction ops every member computes alike, the quick row, the
//! tap verdict and the chips under a message.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/reaction_rules.rs`. Nothing here is
//! enforced on anyone else's device; it decides what this device shows.

use lakko_ui_rules::emoji::{
    emoji_only_count as rust_emoji_only_count, is_single_emoji as rust_is_single_emoji,
    EMOJI_ONLY_MAX,
};
use lakko_ui_rules::reaction::{
    chip_sources_from_held as rust_chip_sources_from_held, fold_reactions as rust_fold_reactions,
    quick_reaction_row as rust_quick_reaction_row, reaction_chips as rust_reaction_chips,
    reaction_shown as rust_reaction_shown, reaction_tap_verdict as rust_reaction_tap_verdict,
    row_has_expand as rust_row_has_expand, ChipConversation, ChipFace, ChipSource, HeldReaction,
    ReactionChange, ReactionOp, ReactionTapVerdict, ReactorKey, DEFAULT_QUICK_REACTION,
    MAX_REACTIONS_PER_MESSAGE, MAX_REACTION_KINDS_SHOWN, QUICK_REACTIONS, QUICK_ROW_LEN,
};

use crate::attachments::ReactionAction;
use crate::conversation_meta::FfiAllowedReactions;
use crate::FfiError;

fn reactor(bytes: &[u8]) -> Result<ReactorKey, FfiError> {
    ReactorKey::try_from(bytes).map_err(|_| {
        FfiError::Attachment(format!(
            "badLength: a reactor account key must be 32 bytes, got {}",
            bytes.len()
        ))
    })
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

// ============================================================================
// Emoji
// ============================================================================

/// Whether `text` is exactly one emoji (no surrounding white space).
#[uniffi::export]
#[must_use]
pub fn is_single_emoji(text: String) -> bool {
    rust_is_single_emoji(&text)
}

/// How many emoji `text` holds when it is 1 to [`emoji_only_max`] emoji and
/// white space only; `0` otherwise. Draw such a message large only when it
/// also has no attachment, reply quote or forward line.
#[uniffi::export]
#[must_use]
pub fn emoji_only_count(text: String) -> u8 {
    rust_emoji_only_count(&text)
}

/// The most emoji a message drawn large holds (3).
#[uniffi::export]
#[must_use]
pub const fn emoji_only_max() -> u8 {
    EMOJI_ONLY_MAX
}

// ============================================================================
// Reactions
// ============================================================================

/// One reaction op on one message, as this device decoded it.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiReactionOp {
    /// The 32-byte account key the op's MLS sender resolved to at receipt.
    pub person: Vec<u8>,
    /// The emoji, as the parser accepted it.
    pub emoji: String,
    /// Add or remove.
    pub change: ReactionAction,
    /// The op's DS message id; ops apply in ascending order.
    pub at: i64,
}

/// A reaction a person holds on a message after the fold.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiHeldReaction {
    /// The 32-byte account key.
    pub person: Vec<u8>,
    /// The emoji.
    pub emoji: String,
    /// The DS message id of the add that gave it.
    pub at: i64,
}

impl From<HeldReaction> for FfiHeldReaction {
    fn from(h: HeldReaction) -> Self {
        Self {
            person: h.person.to_vec(),
            emoji: h.emoji,
            at: h.at,
        }
    }
}

impl TryFrom<FfiHeldReaction> for HeldReaction {
    type Error = FfiError;

    fn try_from(h: FfiHeldReaction) -> Result<Self, FfiError> {
        Ok(Self {
            person: reactor(&h.person)?,
            emoji: h.emoji,
            at: h.at,
        })
    }
}

/// Fold one message's reaction ops (UX sections 10 and 17): per person, an
/// add of an emoji already held and a remove of one not held are no-ops,
/// and an add past [`max_reactions_per_message`] is ignored for good.
/// Returns what is held, ordered by the add that gave it.
///
/// # Errors
///
/// [`FfiError::Attachment`] when a person key is not 32 bytes.
#[uniffi::export]
pub fn fold_reactions(ops: Vec<FfiReactionOp>) -> Result<Vec<FfiHeldReaction>, FfiError> {
    let ops = ops
        .into_iter()
        .map(|o| {
            Ok(ReactionOp {
                person: reactor(&o.person)?,
                emoji: o.emoji,
                change: match o.change {
                    ReactionAction::Add => ReactionChange::Add,
                    ReactionAction::Remove => ReactionChange::Remove,
                },
                at: o.at,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    Ok(rust_fold_reactions(&ops)
        .into_iter()
        .map(Into::into)
        .collect())
}

/// Whether a reaction `emoji` is shown under `allowed` (allowed and one
/// emoji).
#[uniffi::export]
#[must_use]
pub fn reaction_shown(emoji: String, allowed: FfiAllowedReactions) -> bool {
    rust_reaction_shown(&emoji, &allowed.into())
}

/// The quick reaction row for a message: the account's quick reaction
/// (`None` = the default), the recent ones, the defaults, and once the
/// message holds the most kinds, only kinds it already holds.
#[uniffi::export]
#[must_use]
pub fn quick_reaction_row(
    quick: Option<String>,
    recent: Vec<String>,
    allowed: FfiAllowedReactions,
    message_kinds: Vec<String>,
) -> Vec<String> {
    rust_quick_reaction_row(quick.as_deref(), &recent, &allowed.into(), &message_kinds)
}

/// Whether the quick row ends with the control that opens the full picker.
#[uniffi::export]
#[must_use]
pub fn row_has_expand(allowed: FfiAllowedReactions) -> bool {
    rust_row_has_expand(&allowed.into())
}

/// What tapping `emoji` on a message does.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiReactionTapVerdict {
    /// Add it.
    Add,
    /// Take it off (this person holds it).
    Remove,
    /// Not allowed here; do nothing.
    NotAllowed,
    /// This person already holds the most reactions on the message.
    CapReached,
    /// The message already holds the most kinds and this is a new one.
    KindLimit,
}

impl From<ReactionTapVerdict> for FfiReactionTapVerdict {
    fn from(v: ReactionTapVerdict) -> Self {
        match v {
            ReactionTapVerdict::Add => Self::Add,
            ReactionTapVerdict::Remove => Self::Remove,
            ReactionTapVerdict::NotAllowed => Self::NotAllowed,
            ReactionTapVerdict::CapReached => Self::CapReached,
            ReactionTapVerdict::KindLimit => Self::KindLimit,
        }
    }
}

/// The verdict of a tap on `emoji`, given this person's own reactions on
/// the message, the allowed set, and the kinds the message holds.
#[uniffi::export]
#[must_use]
pub fn reaction_tap_verdict(
    emoji: String,
    own: Vec<String>,
    allowed: FfiAllowedReactions,
    message_kinds: Vec<String>,
) -> FfiReactionTapVerdict {
    rust_reaction_tap_verdict(&emoji, &own, &allowed.into(), &message_kinds).into()
}

/// Where the chips are drawn: their face differs.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiChipConversation {
    /// A one-to-one: emoji alone, a count from two.
    OneToOne,
    /// A group: up to three faces, then a count.
    Group,
    /// A channel: always a count.
    Channel,
}

impl From<FfiChipConversation> for ChipConversation {
    fn from(c: FfiChipConversation) -> Self {
        match c {
            FfiChipConversation::OneToOne => Self::OneToOne,
            FfiChipConversation::Group => Self::Group,
            FfiChipConversation::Channel => Self::Channel,
        }
    }
}

/// One emoji's tally under a message, the input of [`reaction_chips`].
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiChipSource {
    /// The emoji.
    pub emoji: String,
    /// How many people hold it.
    pub count: u32,
    /// The earliest DS message id that gave it (orders ties).
    pub first_at: i64,
    /// Whether this person holds it.
    pub mine: bool,
    /// The 32-byte account keys of who holds it, when known (groups).
    pub reactors: Vec<Vec<u8>>,
}

impl From<ChipSource> for FfiChipSource {
    fn from(s: ChipSource) -> Self {
        Self {
            emoji: s.emoji,
            count: s.count,
            first_at: s.first_at,
            mine: s.mine,
            reactors: s.reactors.iter().map(|k| k.to_vec()).collect(),
        }
    }
}

/// What a chip shows beside its emoji.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiChipFace {
    /// The emoji alone.
    EmojiOnly,
    /// A number.
    Count {
        /// The number.
        count: u32,
    },
    /// Small pictures of these people (32-byte account keys).
    Avatars {
        /// Who.
        reactors: Vec<Vec<u8>>,
    },
}

/// One chip under a message.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiReactionChip {
    /// The emoji.
    pub emoji: String,
    /// How many people hold it.
    pub count: u32,
    /// Whether this person holds it (draw it selected).
    pub mine: bool,
    /// What it shows.
    pub face: FfiChipFace,
}

/// The chips under one message.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiChipRow {
    /// The chips, in order.
    pub chips: Vec<FfiReactionChip>,
    /// How many more kinds fold into a last `+N` chip (0 = none).
    pub overflow_kinds: u32,
}

/// Tally held reactions into chip sources, from this person's point of view
/// (`own` is this account's 32-byte key).
///
/// # Errors
///
/// [`FfiError::Attachment`] when a key is not 32 bytes.
#[uniffi::export]
pub fn chip_sources_from_held(
    held: Vec<FfiHeldReaction>,
    own: Vec<u8>,
) -> Result<Vec<FfiChipSource>, FfiError> {
    let held = held
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<HeldReaction>, _>>()?;
    Ok(rust_chip_sources_from_held(&held, &reactor(&own)?)
        .into_iter()
        .map(Into::into)
        .collect())
}

/// The chips to draw under a message: shown ones only, most held first, at
/// most [`max_reaction_kinds_shown`] then an overflow count.
///
/// # Errors
///
/// [`FfiError::Attachment`] when a reactor key is not 32 bytes.
#[uniffi::export]
pub fn reaction_chips(
    sources: Vec<FfiChipSource>,
    conversation: FfiChipConversation,
    allowed: FfiAllowedReactions,
) -> Result<FfiChipRow, FfiError> {
    let sources = sources
        .into_iter()
        .map(|s| {
            Ok(ChipSource {
                emoji: s.emoji,
                count: s.count,
                first_at: s.first_at,
                mine: s.mine,
                reactors: s
                    .reactors
                    .iter()
                    .map(|k| reactor(k))
                    .collect::<Result<Vec<_>, FfiError>>()?,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    let row = rust_reaction_chips(&sources, conversation.into(), &allowed.into());
    Ok(FfiChipRow {
        chips: row
            .chips
            .into_iter()
            .map(|c| FfiReactionChip {
                emoji: c.emoji,
                count: c.count,
                mine: c.mine,
                face: match c.face {
                    ChipFace::EmojiOnly => FfiChipFace::EmojiOnly,
                    ChipFace::Count(count) => FfiChipFace::Count { count },
                    ChipFace::Avatars(r) => FfiChipFace::Avatars {
                        reactors: r.iter().map(|k| k.to_vec()).collect(),
                    },
                },
            })
            .collect(),
        overflow_kinds: row.overflow_kinds,
    })
}

/// The quick reaction row's defaults, in order.
#[uniffi::export]
#[must_use]
pub fn quick_reactions() -> Vec<String> {
    QUICK_REACTIONS.iter().map(|e| (*e).to_string()).collect()
}

/// The quick reaction a person has until they pick another.
#[uniffi::export]
#[must_use]
pub fn default_quick_reaction() -> String {
    DEFAULT_QUICK_REACTION.to_string()
}

/// How many emoji the quick row holds (6).
#[uniffi::export]
#[must_use]
pub fn quick_row_len() -> u32 {
    len_u32(QUICK_ROW_LEN)
}

/// The most reactions one person gives one message (3).
#[uniffi::export]
#[must_use]
pub fn max_reactions_per_message() -> u32 {
    len_u32(MAX_REACTIONS_PER_MESSAGE)
}

/// The most kinds of reaction shown under one message (11).
#[uniffi::export]
#[must_use]
pub fn max_reaction_kinds_shown() -> u32 {
    len_u32(MAX_REACTION_KINDS_SHOWN)
}

#[cfg(test)]
mod tests {
    use super::*;

    const THUMBS: &str = "\u{1F44D}";

    #[test]
    fn emoji_rules_cross_the_boundary() {
        assert!(is_single_emoji(THUMBS.into()));
        assert!(!is_single_emoji(format!(" {THUMBS}")));
        assert_eq!(emoji_only_count(format!("{THUMBS} {THUMBS}")), 2);
        assert_eq!(emoji_only_count("hi".into()), 0);
        assert_eq!(emoji_only_max(), 3);
    }

    #[test]
    fn fold_chips_row_and_verdict_agree() {
        let alice = vec![1_u8; 32];
        let bob = vec![2_u8; 32];
        let held = fold_reactions(vec![
            FfiReactionOp {
                person: alice.clone(),
                emoji: THUMBS.into(),
                change: ReactionAction::Add,
                at: 1,
            },
            FfiReactionOp {
                person: bob.clone(),
                emoji: THUMBS.into(),
                change: ReactionAction::Add,
                at: 2,
            },
            FfiReactionOp {
                person: bob,
                emoji: THUMBS.into(),
                change: ReactionAction::Remove,
                at: 3,
            },
        ])
        .unwrap();
        assert_eq!(held.len(), 1);
        let sources = chip_sources_from_held(held, alice.clone()).unwrap();
        assert!(sources[0].mine);
        let row = reaction_chips(
            sources,
            FfiChipConversation::Group,
            FfiAllowedReactions::All,
        )
        .unwrap();
        assert_eq!(row.chips.len(), 1);
        assert_eq!(
            row.chips[0].face,
            FfiChipFace::Avatars {
                reactors: vec![alice]
            }
        );
        assert_eq!(
            reaction_tap_verdict(
                THUMBS.into(),
                vec![THUMBS.into()],
                FfiAllowedReactions::All,
                Vec::new()
            ),
            FfiReactionTapVerdict::Remove
        );
        assert_eq!(
            reaction_tap_verdict(
                THUMBS.into(),
                Vec::new(),
                FfiAllowedReactions::Off,
                Vec::new()
            ),
            FfiReactionTapVerdict::NotAllowed
        );
        let row = quick_reaction_row(None, Vec::new(), FfiAllowedReactions::All, Vec::new());
        assert_eq!(row.len(), quick_row_len() as usize);
        assert_eq!(row[0], default_quick_reaction());
        assert!(row_has_expand(FfiAllowedReactions::All));
        assert!(!row_has_expand(FfiAllowedReactions::Off));
        assert_eq!(quick_reactions().len(), 6);
        assert_eq!(max_reactions_per_message(), 3);
        assert_eq!(max_reaction_kinds_shown(), 11);
        assert!(fold_reactions(vec![FfiReactionOp {
            person: vec![1; 31],
            emoji: THUMBS.into(),
            change: ReactionAction::Add,
            at: 1,
        }])
        .is_err());
    }
}
