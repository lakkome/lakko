// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the emoji and reaction presentation rules
//! (`lakko_ui_rules::{emoji, reaction}`, UX `docs/ux/reactions-and-emoji.md`
//! sections 17 and 18).
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/reaction_rules.rs`. A change is
//! `"add"` or `"remove"`; a chip face is `"emoji"`, `"count"` or
//! `"avatars"`; a conversation is `"one-to-one"`, `"group"` or `"channel"`;
//! a tap verdict is `"add"`, `"remove"`, `"not-allowed"`, `"cap-reached"`
//! or `"kind-limit"`. Lists of 32-byte keys cross as one flat
//! concatenation.
//!
//! # Errors
//!
//! Every fallible function throws a string `"reaction.<code>: <reason>"`.

use lakko_ui_rules::emoji::{
    emoji_only_count as rust_emoji_only_count, is_single_emoji as rust_is_single_emoji,
    EMOJI_ONLY_MAX,
};
use lakko_ui_rules::reaction::{
    chip_sources_from_held as rust_chip_sources_from_held, fold_reactions as rust_fold_reactions,
    quick_reaction_row as rust_quick_reaction_row, reaction_chips as rust_reaction_chips,
    reaction_shown as rust_reaction_shown, reaction_tap_verdict as rust_reaction_tap_verdict,
    row_has_expand as rust_row_has_expand, ChipConversation, ChipFace,
    ChipSource as RustChipSource, HeldReaction as RustHeldReaction, ReactionChange,
    ReactionOp as RustReactionOp, ReactionTapVerdict, ReactorKey, DEFAULT_QUICK_REACTION,
    MAX_REACTIONS_PER_MESSAGE, MAX_REACTION_KINDS_SHOWN, QUICK_REACTIONS, QUICK_ROW_LEN,
};
use wasm_bindgen::prelude::*;

use crate::conversation_meta::WasmAllowedReactions;

fn reactor(bytes: &[u8]) -> Result<ReactorKey, String> {
    ReactorKey::try_from(bytes).map_err(|_| {
        format!(
            "reaction.badLength: a reactor account key must be 32 bytes, got {}",
            bytes.len()
        )
    })
}

fn reactors(flat: &[u8]) -> Result<Vec<ReactorKey>, String> {
    if flat.len() % 32 != 0 {
        return Err("reaction.badLength: reactors must be 32-byte keys concatenated".to_string());
    }
    flat.chunks_exact(32).map(reactor).collect()
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Whether `text` is exactly one emoji.
#[wasm_bindgen(js_name = isSingleEmoji)]
#[must_use]
pub fn is_single_emoji(text: &str) -> bool {
    rust_is_single_emoji(text)
}

/// 1 to 3 when `text` is that many emoji and white space only, else 0.
#[wasm_bindgen(js_name = emojiOnlyCount)]
#[must_use]
pub fn emoji_only_count(text: &str) -> u8 {
    rust_emoji_only_count(text)
}

/// The most emoji a message drawn large holds (3).
#[wasm_bindgen(js_name = emojiOnlyMax)]
#[must_use]
pub fn emoji_only_max() -> u8 {
    EMOJI_ONLY_MAX
}

/// One reaction op on one message.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionOp {
    inner: RustReactionOp,
}

#[wasm_bindgen]
impl ReactionOp {
    /// `person` the 32-byte account key resolved at receipt, `change`
    /// `"add"` / `"remove"`, `at` the op's DS message id.
    ///
    /// # Errors
    ///
    /// Throws `reaction.badLength` / `reaction.badChange`.
    #[wasm_bindgen(constructor)]
    pub fn new(person: &[u8], emoji: String, change: &str, at: i64) -> Result<ReactionOp, String> {
        let change = match change {
            "add" => ReactionChange::Add,
            "remove" => ReactionChange::Remove,
            _ => return Err("reaction.badChange: change must be \"add\" or \"remove\"".to_string()),
        };
        Ok(Self {
            inner: RustReactionOp {
                person: reactor(person)?,
                emoji,
                change,
                at,
            },
        })
    }
}

/// A reaction a person holds after the fold.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldReaction {
    inner: RustHeldReaction,
}

#[wasm_bindgen]
impl HeldReaction {
    /// Rebuild a held reaction (for a cached fold).
    ///
    /// # Errors
    ///
    /// Throws `reaction.badLength`.
    #[wasm_bindgen(constructor)]
    pub fn new(person: &[u8], emoji: String, at: i64) -> Result<HeldReaction, String> {
        Ok(Self {
            inner: RustHeldReaction {
                person: reactor(person)?,
                emoji,
                at,
            },
        })
    }

    /// The 32-byte account key.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn person(&self) -> Vec<u8> {
        self.inner.person.to_vec()
    }

    /// The emoji.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn emoji(&self) -> String {
        self.inner.emoji.clone()
    }

    /// The DS message id of the add that gave it.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn at(&self) -> i64 {
        self.inner.at
    }
}

/// Fold one message's reaction ops (UX sections 10 and 17).
#[wasm_bindgen(js_name = foldReactions)]
#[must_use]
pub fn fold_reactions(ops: Vec<ReactionOp>) -> Vec<HeldReaction> {
    let ops: Vec<RustReactionOp> = ops.into_iter().map(|o| o.inner).collect();
    rust_fold_reactions(&ops)
        .into_iter()
        .map(|inner| HeldReaction { inner })
        .collect()
}

/// Whether a reaction `emoji` is shown under `allowed`.
#[wasm_bindgen(js_name = reactionShown)]
#[must_use]
pub fn reaction_shown(emoji: &str, allowed: &WasmAllowedReactions) -> bool {
    rust_reaction_shown(emoji, &allowed.inner)
}

/// The quick reaction row for a message.
#[wasm_bindgen(js_name = quickReactionRow)]
#[must_use]
pub fn quick_reaction_row(
    quick: Option<String>,
    recent: Vec<String>,
    allowed: &WasmAllowedReactions,
    message_kinds: Vec<String>,
) -> Vec<String> {
    rust_quick_reaction_row(quick.as_deref(), &recent, &allowed.inner, &message_kinds)
}

/// Whether the quick row ends with the control that opens the picker.
#[wasm_bindgen(js_name = rowHasExpand)]
#[must_use]
pub fn row_has_expand(allowed: &WasmAllowedReactions) -> bool {
    rust_row_has_expand(&allowed.inner)
}

/// What tapping `emoji` does: `"add"`, `"remove"`, `"not-allowed"`,
/// `"cap-reached"` or `"kind-limit"`.
#[wasm_bindgen(js_name = reactionTapVerdict)]
#[must_use]
pub fn reaction_tap_verdict(
    emoji: &str,
    own: Vec<String>,
    allowed: &WasmAllowedReactions,
    message_kinds: Vec<String>,
) -> String {
    match rust_reaction_tap_verdict(emoji, &own, &allowed.inner, &message_kinds) {
        ReactionTapVerdict::Add => "add",
        ReactionTapVerdict::Remove => "remove",
        ReactionTapVerdict::NotAllowed => "not-allowed",
        ReactionTapVerdict::CapReached => "cap-reached",
        ReactionTapVerdict::KindLimit => "kind-limit",
    }
    .to_string()
}

/// One emoji's tally under a message.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChipSource {
    inner: RustChipSource,
}

#[wasm_bindgen]
impl ChipSource {
    /// Build a tally (`reactors` = 32-byte keys concatenated, may be empty).
    ///
    /// # Errors
    ///
    /// Throws `reaction.badLength`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        emoji: String,
        count: u32,
        first_at: i64,
        mine: bool,
        reactors_flat: &[u8],
    ) -> Result<ChipSource, String> {
        Ok(Self {
            inner: RustChipSource {
                emoji,
                count,
                first_at,
                mine,
                reactors: reactors(reactors_flat)?,
            },
        })
    }

    /// The emoji.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn emoji(&self) -> String {
        self.inner.emoji.clone()
    }

    /// How many people hold it.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn count(&self) -> u32 {
        self.inner.count
    }

    /// Whether this person holds it.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn mine(&self) -> bool {
        self.inner.mine
    }
}

/// Tally held reactions from this person's (`own`) point of view.
///
/// # Errors
///
/// Throws `reaction.badLength` when `own` is not 32 bytes.
#[wasm_bindgen(js_name = chipSourcesFromHeld)]
pub fn chip_sources_from_held(
    held: Vec<HeldReaction>,
    own: &[u8],
) -> Result<Vec<ChipSource>, String> {
    let held: Vec<RustHeldReaction> = held.into_iter().map(|h| h.inner).collect();
    Ok(rust_chip_sources_from_held(&held, &reactor(own)?)
        .into_iter()
        .map(|inner| ChipSource { inner })
        .collect())
}

/// One chip under a message.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionChip {
    emoji: String,
    count: u32,
    mine: bool,
    face: ChipFace,
}

#[wasm_bindgen]
impl ReactionChip {
    /// The emoji.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn emoji(&self) -> String {
        self.emoji.clone()
    }

    /// How many people hold it.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Whether this person holds it.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn mine(&self) -> bool {
        self.mine
    }

    /// `"emoji"`, `"count"` or `"avatars"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn face(&self) -> String {
        match self.face {
            ChipFace::EmojiOnly => "emoji",
            ChipFace::Count(_) => "count",
            ChipFace::Avatars(_) => "avatars",
        }
        .to_string()
    }

    /// The people of an `"avatars"` face, 32-byte keys concatenated; empty
    /// otherwise.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn reactors(&self) -> Vec<u8> {
        match &self.face {
            ChipFace::Avatars(r) => r.concat(),
            _ => Vec::new(),
        }
    }
}

/// The chips under one message.
#[wasm_bindgen]
pub struct ChipRow {
    chips: Vec<ReactionChip>,
    overflow_kinds: u32,
}

#[wasm_bindgen]
impl ChipRow {
    /// The chips, in order.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn chips(&self) -> Vec<ReactionChip> {
        self.chips.clone()
    }

    /// How many more kinds fold into a last `+N` chip.
    #[wasm_bindgen(getter, js_name = overflowKinds)]
    #[must_use]
    pub fn overflow_kinds(&self) -> u32 {
        self.overflow_kinds
    }
}

/// The chips to draw under a message; `conversation` is `"one-to-one"`,
/// `"group"` or `"channel"`.
///
/// # Errors
///
/// Throws `reaction.badConversation`.
#[wasm_bindgen(js_name = reactionChips)]
pub fn reaction_chips(
    sources: Vec<ChipSource>,
    conversation: &str,
    allowed: &WasmAllowedReactions,
) -> Result<ChipRow, String> {
    let conversation = match conversation {
        "one-to-one" => ChipConversation::OneToOne,
        "group" => ChipConversation::Group,
        "channel" => ChipConversation::Channel,
        _ => return Err("reaction.badConversation: one-to-one, group or channel".to_string()),
    };
    let sources: Vec<RustChipSource> = sources.into_iter().map(|s| s.inner).collect();
    let row = rust_reaction_chips(&sources, conversation, &allowed.inner);
    Ok(ChipRow {
        chips: row
            .chips
            .into_iter()
            .map(|c| ReactionChip {
                emoji: c.emoji,
                count: c.count,
                mine: c.mine,
                face: c.face,
            })
            .collect(),
        overflow_kinds: row.overflow_kinds,
    })
}

/// The quick reaction row's defaults.
#[wasm_bindgen(js_name = quickReactions)]
#[must_use]
pub fn quick_reactions() -> Vec<String> {
    QUICK_REACTIONS.iter().map(|e| (*e).to_string()).collect()
}

/// The quick reaction until a person picks another.
#[wasm_bindgen(js_name = defaultQuickReaction)]
#[must_use]
pub fn default_quick_reaction() -> String {
    DEFAULT_QUICK_REACTION.to_string()
}

/// How many emoji the quick row holds.
#[wasm_bindgen(js_name = quickRowLen)]
#[must_use]
pub fn quick_row_len() -> u32 {
    len_u32(QUICK_ROW_LEN)
}

/// The most reactions one person gives one message.
#[wasm_bindgen(js_name = maxReactionsPerMessage)]
#[must_use]
pub fn max_reactions_per_message() -> u32 {
    len_u32(MAX_REACTIONS_PER_MESSAGE)
}

/// The most kinds of reaction shown under one message.
#[wasm_bindgen(js_name = maxReactionKindsShown)]
#[must_use]
pub fn max_reaction_kinds_shown() -> u32 {
    len_u32(MAX_REACTION_KINDS_SHOWN)
}

#[cfg(test)]
mod tests {
    use super::*;

    const THUMBS: &str = "\u{1F44D}";

    #[test]
    fn rules_cross_the_boundary() {
        assert!(is_single_emoji(THUMBS));
        assert_eq!(emoji_only_count("hi"), 0);
        let held = fold_reactions(vec![
            ReactionOp::new(&[1; 32], THUMBS.into(), "add", 1).unwrap(),
            ReactionOp::new(&[2; 32], THUMBS.into(), "add", 2).unwrap(),
            ReactionOp::new(&[2; 32], THUMBS.into(), "remove", 3).unwrap(),
        ]);
        assert_eq!(held.len(), 1);
        let sources = chip_sources_from_held(held, &[1; 32]).unwrap();
        assert!(sources[0].mine());
        let row = reaction_chips(sources, "group", &WasmAllowedReactions::all()).unwrap();
        assert_eq!(row.chips()[0].face(), "avatars");
        assert_eq!(row.chips()[0].reactors(), vec![1; 32]);
        assert_eq!(
            reaction_tap_verdict(THUMBS, Vec::new(), &WasmAllowedReactions::off(), Vec::new()),
            "not-allowed"
        );
        let row = quick_reaction_row(None, Vec::new(), &WasmAllowedReactions::all(), Vec::new());
        assert_eq!(row[0], default_quick_reaction());
        assert!(ReactionOp::new(&[1; 32], THUMBS.into(), "toggle", 1).is_err());
        assert!(reaction_chips(Vec::new(), "dm", &WasmAllowedReactions::all()).is_err());
    }
}
