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
//! A channel owner, or a group's owner and admins, may narrow what the
//! conversation takes ([`AllowedReactions`], carried by the channel metadata
//! record and the group allowed-reactions envelope); a client then drops from
//! this row what the conversation does not allow.
//!
//! ## The rules here (UX `docs/ux/reactions-and-emoji.md` sections 5-10, 17)
//!
//! * [`fold_reactions`] — what each person holds on one message, from the
//!   add / remove ops in journal order, at most
//!   [`MAX_REACTIONS_PER_MESSAGE`] per person. Every member computes it alike.
//! * [`quick_reaction_row`] — the six emoji the menu offers first.
//! * [`reaction_tap_verdict`] — what pressing an emoji (row, chip, double tap)
//!   does for this person on this message.
//! * [`reaction_chips`] — the chips under a message: order, face, hiding, the
//!   cap of [`MAX_REACTION_KINDS_SHOWN`] kinds.
//!
//! ### Why the fold is here and not beside the chat body parser
//!
//! The parser (`lakko_attachments::parse_body`) decides what ONE message
//! says; the fold decides what a SEQUENCE of them means for a chip row, keyed
//! on account keys the client resolves from MLS senders (ADR-0050). It needs
//! nothing from the wire crate, no background process needs it (the iOS
//! notification extension links `lakko-attachments`, not this crate), and the
//! three clients must agree on its answer, which is what this crate is for.
//! It decides what people see, not what anyone may do: nothing here is
//! enforced on anyone else's device.

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

use std::collections::BTreeMap;

pub use lakko_contacts::allowed_reactions::AllowedReactions;

use crate::emoji::is_single_emoji;

/// The quick reaction a person has until they pick another (UX section 6).
pub const DEFAULT_QUICK_REACTION: &str = QUICK_REACTIONS[0];

/// How many emoji the reaction row holds (UX section 5).
pub const QUICK_ROW_LEN: usize = 6;

/// The most kinds of reaction shown under one message; the rest fold into a
/// last `+N` chip, and a sending client adds no new kind once a message holds
/// this many (UX D6; Telegram's default).
pub const MAX_REACTION_KINDS_SHOWN: usize = 11;

/// Length of the account key a reaction is keyed on.
pub const REACTOR_KEY_LEN: usize = 32;

/// An account key: the person a reaction belongs to (ADR-0050: the account a
/// leaf resolves to, never a device's leaf key).
pub type ReactorKey = [u8; REACTOR_KEY_LEN];

/// Add or take off, as the reaction op said (`lakko_attachments::ReactionAction`
/// on the wire; restated here so this crate needs no wire dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReactionChange {
    /// Add the emoji to the person's reactions on the message.
    Add,
    /// Take the emoji off the person's reactions on the message.
    Remove,
}

/// One reaction op on one message, as the client decoded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionOp {
    /// The account key the op's MLS sender resolved to at receipt (C-68,
    /// ADR-0050).
    pub person: ReactorKey,
    /// The emoji, as the parser accepted it.
    pub emoji: String,
    /// Add or remove.
    pub change: ReactionChange,
    /// The op's position in the conversation's journal: its DS message id.
    /// Ops are applied in ascending order; equal values keep input order.
    pub at: i64,
}

/// A reaction a person holds on a message after the fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldReaction {
    /// The person (account key).
    pub person: ReactorKey,
    /// The emoji.
    pub emoji: String,
    /// The journal position of the add that gave it.
    pub at: i64,
}

/// The fold every member computes alike (UX section 10 and section 17 item
/// 3): apply one message's reaction ops in journal order, per person:
///
/// * an add of an emoji the person already holds is a no-op;
/// * a remove of an emoji the person does not hold is a no-op;
/// * an add that would give the person a fourth reaction
///   ([`MAX_REACTIONS_PER_MESSAGE`]) is ignored, and stays ignored: it takes
///   no place freed later; only a later add of its own counts.
///
/// The fold looks at no emoji: whether a reaction is shown (allowed here, an
/// emoji at all) is [`reaction_chips`]'s question, so a setting changed later
/// changes what is shown, never what is held (UX D11).
///
/// Returns what is held, ordered by the journal position of the add that gave
/// it (input order among equal positions).
#[must_use]
pub fn fold_reactions(ops: &[ReactionOp]) -> Vec<HeldReaction> {
    let mut order: Vec<usize> = (0..ops.len()).collect();
    order.sort_by_key(|&i| ops[i].at);
    // Per person, the held emoji with (journal position, sequence number).
    let mut held: BTreeMap<ReactorKey, Vec<(&str, i64, usize)>> = BTreeMap::new();
    for (seq, i) in order.into_iter().enumerate() {
        let op = &ops[i];
        let mine = held.entry(op.person).or_default();
        let pos = mine.iter().position(|(e, _, _)| *e == op.emoji);
        match (op.change, pos) {
            (ReactionChange::Add, None) if mine.len() < MAX_REACTIONS_PER_MESSAGE => {
                mine.push((op.emoji.as_str(), op.at, seq));
            },
            (ReactionChange::Remove, Some(p)) => {
                mine.remove(p);
            },
            _ => {},
        }
    }
    let mut out: Vec<(usize, HeldReaction)> = held
        .into_iter()
        .flat_map(|(person, list)| {
            list.into_iter().map(move |(emoji, at, seq)| {
                (
                    seq,
                    HeldReaction {
                        person,
                        emoji: emoji.to_string(),
                        at,
                    },
                )
            })
        })
        .collect();
    out.sort_by_key(|(seq, _)| *seq);
    out.into_iter().map(|(_, h)| h).collect()
}

/// Whether a conversation shows a reaction: it allows it and it is one emoji
/// ([`crate::emoji::is_single_emoji`]).
#[must_use]
pub fn reaction_shown(emoji: &str, allowed: &AllowedReactions) -> bool {
    allowed.allows(emoji) && is_single_emoji(emoji)
}

/// The reaction row (UX section 5): six emoji, the person's quick reaction
/// first, then their recent reactions, then the defaults ([`QUICK_REACTIONS`])
/// or, when the conversation takes only a list, that list in the owner's
/// order; no repeats; only what the conversation shows ([`reaction_shown`]).
/// Empty when the conversation takes no reactions.
///
/// `quick` is the person's quick reaction (`None` = not chosen,
/// [`DEFAULT_QUICK_REACTION`]); `recent` is the picker's Recent list,
/// reactions only, newest first; `message_kinds` are the kinds of reaction
/// shown on the message now. Once the message shows
/// [`MAX_REACTION_KINDS_SHOWN`] kinds, the row offers only those kinds (UX
/// D6).
#[must_use]
pub fn quick_reaction_row(
    quick: Option<&str>,
    recent: &[String],
    allowed: &AllowedReactions,
    message_kinds: &[String],
) -> Vec<String> {
    let full = message_kinds.len() >= MAX_REACTION_KINDS_SHOWN;
    let defaults: Vec<&str> = match allowed {
        AllowedReactions::Off => return Vec::new(),
        AllowedReactions::All => QUICK_REACTIONS.to_vec(),
        AllowedReactions::Only(list) => list.iter().map(String::as_str).collect(),
    };
    let candidates = core::iter::once(quick.unwrap_or(DEFAULT_QUICK_REACTION))
        .chain(recent.iter().map(String::as_str))
        .chain(defaults)
        .chain(message_kinds.iter().map(String::as_str).filter(|_| full));
    let mut row: Vec<String> = Vec::with_capacity(QUICK_ROW_LEN);
    for e in candidates {
        if row.len() == QUICK_ROW_LEN {
            break;
        }
        if !reaction_shown(e, allowed)
            || row.iter().any(|r| r == e)
            || (full && !message_kinds.iter().any(|k| k == e))
        {
            continue;
        }
        row.push(e.to_string());
    }
    row
}

/// Whether the row shows its trailing expand control (UX section 5): not when
/// the conversation takes no reactions, nor when it takes a list of
/// [`QUICK_ROW_LEN`] or fewer (the row already holds all of them).
#[must_use]
pub fn row_has_expand(allowed: &AllowedReactions) -> bool {
    match allowed {
        AllowedReactions::All => true,
        AllowedReactions::Off => false,
        AllowedReactions::Only(list) => list.len() > QUICK_ROW_LEN,
    }
}

/// What pressing `emoji` does for this person on this message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionTapVerdict {
    /// Send an add.
    Add,
    /// The person holds it: send a remove. Always possible, even for a
    /// reaction the conversation no longer takes.
    Remove,
    /// Not taken here (or not an emoji): nothing happens, silently.
    NotAllowed,
    /// The person already holds [`MAX_REACTIONS_PER_MESSAGE`]: nothing is
    /// sent; the client shows "You can add up to 3 reactions. Take one off
    /// to add another."
    CapReached,
    /// The message already shows [`MAX_REACTION_KINDS_SHOWN`] kinds and this
    /// is a new one: nothing is sent (UX D6).
    KindLimit,
}

/// The one rule behind a row press, a chip tap and a double tap (UX sections
/// 5, 6, 7 and 10). `own` are the emoji this person holds on the message
/// (from [`fold_reactions`]); `message_kinds` the kinds shown on it.
#[must_use]
pub fn reaction_tap_verdict(
    emoji: &str,
    own: &[String],
    allowed: &AllowedReactions,
    message_kinds: &[String],
) -> ReactionTapVerdict {
    if own.iter().any(|e| e == emoji) {
        ReactionTapVerdict::Remove
    } else if !reaction_shown(emoji, allowed) {
        ReactionTapVerdict::NotAllowed
    } else if own.len() >= MAX_REACTIONS_PER_MESSAGE {
        ReactionTapVerdict::CapReached
    } else if message_kinds.len() >= MAX_REACTION_KINDS_SHOWN
        && !message_kinds.iter().any(|k| k == emoji)
    {
        ReactionTapVerdict::KindLimit
    } else {
        ReactionTapVerdict::Add
    }
}

/// Where the chips are drawn: what a chip may show differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipConversation {
    /// A one-to-one chat: the reactors' avatars, as in a group (who reacted
    /// is shown in a one-to-one too, maintainer 2026-11-07).
    OneToOne,
    /// A group: avatars when three or fewer gave it, a count otherwise.
    Group,
    /// A channel: always a count (nobody's avatar can be known).
    Channel,
}

/// One kind of reaction on a message, before the chip rule: what a client
/// knows of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipSource {
    /// The emoji.
    pub emoji: String,
    /// How many people gave it.
    pub count: u32,
    /// When it was first given among those who hold it now (journal position
    /// in a chat; the order the channel service listed it in a channel).
    pub first_at: i64,
    /// Whether this person holds it.
    pub mine: bool,
    /// Who gave it, in the order given; empty where nobody can know (a
    /// channel).
    pub reactors: Vec<ReactorKey>,
}

/// What a chip shows beside its emoji.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipFace {
    /// The emoji alone.
    EmojiOnly,
    /// The emoji and a number.
    Count(u32),
    /// The emoji and these people's avatars, in the order given (one to
    /// three).
    Avatars(Vec<ReactorKey>),
}

/// One chip as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionChip {
    /// The emoji.
    pub emoji: String,
    /// How many people gave it.
    pub count: u32,
    /// Whether this person holds it (drawn with the accent; the screen reader
    /// says "your reaction").
    pub mine: bool,
    /// What it shows beside the emoji.
    pub face: ChipFace,
}

/// The chips under one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipRow {
    /// The chips shown, at most [`MAX_REACTION_KINDS_SHOWN`], in order.
    pub chips: Vec<ReactionChip>,
    /// How many more kinds fold into the last `+N` chip (`0` = no such chip).
    pub overflow_kinds: u32,
}

/// Turn the fold's result into chip sources: one per emoji, counted per
/// person, `first_at` the earliest add among those who hold it.
#[must_use]
pub fn chip_sources_from_held(held: &[HeldReaction], own: &ReactorKey) -> Vec<ChipSource> {
    let mut out: Vec<ChipSource> = Vec::new();
    for h in held {
        if let Some(s) = out.iter_mut().find(|s| s.emoji == h.emoji) {
            s.count = s.count.saturating_add(1);
            s.first_at = s.first_at.min(h.at);
            s.mine |= h.person == *own;
            s.reactors.push(h.person);
        } else {
            out.push(ChipSource {
                emoji: h.emoji.clone(),
                count: 1,
                first_at: h.at,
                mine: h.person == *own,
                reactors: vec![h.person],
            });
        }
    }
    out
}

/// The chip rule (UX section 7):
///
/// * hidden: a kind the conversation does not show ([`reaction_shown`]: not
///   allowed any more, or not one emoji) and a kind nobody gives;
/// * order: most people first; equal counts by `first_at`, then by the
///   emoji's bytes, so every device draws the same row;
/// * at most [`MAX_REACTION_KINDS_SHOWN`] chips; the rest are counted in
///   [`ChipRow::overflow_kinds`] for the `+N` chip;
/// * face: [`ChipConversation`] decides; a one-to-one or a group shows
///   avatars only when it knows every one of the at most three people.
#[must_use]
pub fn reaction_chips(
    sources: &[ChipSource],
    conversation: ChipConversation,
    allowed: &AllowedReactions,
) -> ChipRow {
    let mut shown: Vec<&ChipSource> = sources
        .iter()
        .filter(|s| s.count > 0 && reaction_shown(&s.emoji, allowed))
        .collect();
    shown.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(a.first_at.cmp(&b.first_at))
            .then(a.emoji.as_bytes().cmp(b.emoji.as_bytes()))
    });
    let overflow = shown.len().saturating_sub(MAX_REACTION_KINDS_SHOWN);
    let chips = shown
        .into_iter()
        .take(MAX_REACTION_KINDS_SHOWN)
        .map(|s| ReactionChip {
            emoji: s.emoji.clone(),
            count: s.count,
            mine: s.mine,
            face: match conversation {
                ChipConversation::OneToOne | ChipConversation::Group
                    if s.count <= 3 && s.reactors.len() == s.count as usize =>
                {
                    ChipFace::Avatars(s.reactors.clone())
                },
                ChipConversation::OneToOne | ChipConversation::Group | ChipConversation::Channel => {
                    ChipFace::Count(s.count)
                },
            },
        })
        .collect();
    ChipRow {
        chips,
        overflow_kinds: u32::try_from(overflow).unwrap_or(u32::MAX),
    }
}

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
            assert!(is_single_emoji(e), "{e:?}");
        }
    }

    const A: ReactorKey = [1; 32];
    const B: ReactorKey = [2; 32];
    const C: ReactorKey = [3; 32];
    const D: ReactorKey = [4; 32];

    fn op(person: ReactorKey, emoji: &str, change: ReactionChange, at: i64) -> ReactionOp {
        ReactionOp {
            person,
            emoji: emoji.to_string(),
            change,
            at,
        }
    }

    fn add(person: ReactorKey, emoji: &str, at: i64) -> ReactionOp {
        op(person, emoji, ReactionChange::Add, at)
    }

    fn remove(person: ReactorKey, emoji: &str, at: i64) -> ReactionOp {
        op(person, emoji, ReactionChange::Remove, at)
    }

    fn emojis_of(held: &[HeldReaction], person: ReactorKey) -> Vec<String> {
        held.iter()
            .filter(|h| h.person == person)
            .map(|h| h.emoji.clone())
            .collect()
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    const E1: &str = "\u{1F44D}";
    const E2: &str = "\u{2764}\u{FE0F}";
    const E3: &str = "\u{1F602}";
    const E4: &str = "\u{1F62E}";
    const E5: &str = "\u{1F525}";

    // --- the fold --------------------------------------------------------

    #[test]
    fn an_add_of_a_held_emoji_and_a_remove_of_an_unheld_one_are_no_ops() {
        let held = fold_reactions(&[
            add(A, E1, 1),
            add(A, E1, 2),
            remove(A, E2, 3),
            add(B, E1, 4),
        ]);
        assert_eq!(
            held,
            vec![
                HeldReaction {
                    person: A,
                    emoji: E1.into(),
                    at: 1
                },
                HeldReaction {
                    person: B,
                    emoji: E1.into(),
                    at: 4
                },
            ]
        );
    }

    #[test]
    fn a_fourth_add_is_ignored_and_stays_ignored() {
        let held = fold_reactions(&[
            add(A, E1, 1),
            add(A, E2, 2),
            add(A, E3, 3),
            add(A, E4, 4), // the 4th: ignored
            remove(A, E1, 5),
            // E4 does not take the freed place; a remove of it is a no-op.
            remove(A, E4, 6),
        ]);
        assert_eq!(emojis_of(&held, A), strings(&[E2, E3]));
        // Only a later add of its own counts.
        let held = fold_reactions(&[
            add(A, E1, 1),
            add(A, E2, 2),
            add(A, E3, 3),
            add(A, E4, 4),
            remove(A, E1, 5),
            add(A, E4, 6),
        ]);
        assert_eq!(emojis_of(&held, A), strings(&[E2, E3, E4]));
    }

    #[test]
    fn two_devices_pressing_at_once_do_not_cancel_each_other() {
        // Both of A's devices add the same emoji: one reaction, not none.
        let held = fold_reactions(&[add(A, E1, 1), add(A, E1, 2)]);
        assert_eq!(emojis_of(&held, A), strings(&[E1]));
    }

    #[test]
    fn the_journal_position_orders_the_ops_not_the_input() {
        let held = fold_reactions(&[remove(A, E1, 2), add(A, E1, 1)]);
        assert!(held.is_empty());
        let held = fold_reactions(&[add(A, E1, 2), remove(A, E1, 1)]);
        assert_eq!(emojis_of(&held, A), strings(&[E1]));
    }

    #[test]
    fn the_fold_does_not_judge_the_emoji() {
        // A string that is not an emoji is held (it takes a place); the chip
        // rule hides it.
        let held = fold_reactions(&[add(A, "ok", 1)]);
        assert_eq!(emojis_of(&held, A), strings(&["ok"]));
        let row = reaction_chips(
            &chip_sources_from_held(&held, &B),
            ChipConversation::Group,
            &AllowedReactions::All,
        );
        assert!(row.chips.is_empty());
    }

    // --- chips -------------------------------------------------------------

    #[test]
    fn chips_order_by_count_then_first_given() {
        let held = fold_reactions(&[add(A, E2, 1), add(B, E1, 2), add(C, E1, 3), add(D, E3, 4)]);
        let row = reaction_chips(
            &chip_sources_from_held(&held, &A),
            ChipConversation::Group,
            &AllowedReactions::All,
        );
        let order: Vec<&str> = row.chips.iter().map(|c| c.emoji.as_str()).collect();
        assert_eq!(order, vec![E1, E2, E3]);
        assert_eq!(row.chips[0].face, ChipFace::Avatars(vec![B, C]));
        assert!(row.chips[1].mine);
        assert!(!row.chips[0].mine);
        assert_eq!(row.overflow_kinds, 0);
    }

    #[test]
    fn the_face_follows_the_conversation() {
        let source = |count: u32, reactors: Vec<ReactorKey>| ChipSource {
            emoji: E1.into(),
            count,
            first_at: 0,
            mine: false,
            reactors,
        };
        let face = |conv, s: ChipSource| {
            reaction_chips(&[s], conv, &AllowedReactions::All).chips[0]
                .face
                .clone()
        };
        assert_eq!(
            face(ChipConversation::OneToOne, source(1, vec![A])),
            ChipFace::Avatars(vec![A])
        );
        assert_eq!(
            face(ChipConversation::OneToOne, source(2, vec![A, B])),
            ChipFace::Avatars(vec![A, B])
        );
        // A one-to-one that does not know the reactor shows the number.
        assert_eq!(
            face(ChipConversation::OneToOne, source(1, vec![])),
            ChipFace::Count(1)
        );
        assert_eq!(
            face(ChipConversation::Group, source(3, vec![A, B, C])),
            ChipFace::Avatars(vec![A, B, C])
        );
        assert_eq!(
            face(ChipConversation::Group, source(4, vec![A, B, C, D])),
            ChipFace::Count(4)
        );
        // A group that does not know every reactor shows the number.
        assert_eq!(
            face(ChipConversation::Group, source(2, vec![A])),
            ChipFace::Count(2)
        );
        assert_eq!(
            face(ChipConversation::Channel, source(1, vec![])),
            ChipFace::Count(1)
        );
    }

    #[test]
    fn chips_of_reactions_not_taken_are_hidden_and_come_back() {
        let held = fold_reactions(&[add(A, E1, 1), add(B, E2, 2)]);
        let sources = chip_sources_from_held(&held, &A);
        let only = AllowedReactions::Only(strings(&[E2]));
        let row = reaction_chips(&sources, ChipConversation::Group, &only);
        assert_eq!(row.chips.len(), 1);
        assert_eq!(row.chips[0].emoji, E2);
        assert!(
            reaction_chips(&sources, ChipConversation::Group, &AllowedReactions::Off)
                .chips
                .is_empty()
        );
        // The fold kept E1; allowing it again shows it again.
        assert_eq!(
            reaction_chips(&sources, ChipConversation::Group, &AllowedReactions::All)
                .chips
                .len(),
            2
        );
    }

    #[test]
    fn at_most_eleven_kinds_then_plus_n() {
        let emoji: Vec<String> = (0..14_u32)
            .map(|i| char::from_u32(0x1F600 + i).unwrap().to_string())
            .collect();
        let sources: Vec<ChipSource> = emoji
            .iter()
            .enumerate()
            .map(|(i, e)| ChipSource {
                emoji: e.clone(),
                count: 1,
                first_at: i64::try_from(i).unwrap(),
                mine: false,
                reactors: vec![A],
            })
            .collect();
        let row = reaction_chips(&sources, ChipConversation::Group, &AllowedReactions::All);
        assert_eq!(row.chips.len(), MAX_REACTION_KINDS_SHOWN);
        assert_eq!(row.overflow_kinds, 3);
        assert_eq!(row.chips[0].emoji, emoji[0]);
    }

    // --- the row and the tap ------------------------------------------------

    #[test]
    fn the_row_is_quick_then_recent_then_defaults() {
        let all = AllowedReactions::All;
        assert_eq!(
            quick_reaction_row(None, &[], &all, &[]),
            strings(&QUICK_REACTIONS)
        );
        let row = quick_reaction_row(Some(E5), &strings(&[E3, E5, "ok"]), &all, &[]);
        assert_eq!(row, strings(&[E5, E3, E1, E2, E4, "\u{1F622}"]));
        assert!(quick_reaction_row(None, &[], &AllowedReactions::Off, &[]).is_empty());
    }

    #[test]
    fn a_list_narrows_the_row_in_the_owners_order() {
        let only = AllowedReactions::Only(strings(&[E4, E3, E2]));
        // E5 is not allowed; E3 comes first (recent), then the owner's order.
        let row = quick_reaction_row(Some(E5), &strings(&[E3]), &only, &[]);
        assert_eq!(row, strings(&[E3, E4, E2]));
        assert!(!row_has_expand(&only));
        assert!(row_has_expand(&AllowedReactions::All));
        assert!(!row_has_expand(&AllowedReactions::Off));
    }

    #[test]
    fn eleven_kinds_narrow_the_row_to_those_kinds() {
        let kinds: Vec<String> = (0..11_u32)
            .map(|i| char::from_u32(0x1F630 + i).unwrap().to_string())
            .collect();
        let row = quick_reaction_row(Some(&kinds[5]), &[], &AllowedReactions::All, &kinds);
        assert_eq!(row.len(), QUICK_ROW_LEN);
        assert_eq!(row[0], kinds[5]);
        assert!(row.iter().all(|e| kinds.contains(e)));
    }

    #[test]
    fn the_tap_verdict() {
        let all = AllowedReactions::All;
        let v = |e: &str, own: &[&str], allowed: &AllowedReactions, kinds: &[String]| {
            reaction_tap_verdict(e, &strings(own), allowed, kinds)
        };
        assert_eq!(v(E1, &[], &all, &[]), ReactionTapVerdict::Add);
        assert_eq!(v(E1, &[E1], &all, &[]), ReactionTapVerdict::Remove);
        // Taking off is possible even when it is no longer allowed.
        assert_eq!(
            v(E1, &[E1], &AllowedReactions::Off, &[]),
            ReactionTapVerdict::Remove
        );
        assert_eq!(
            v(E1, &[], &AllowedReactions::Off, &[]),
            ReactionTapVerdict::NotAllowed
        );
        assert_eq!(v("ok", &[], &all, &[]), ReactionTapVerdict::NotAllowed);
        assert_eq!(
            v(E4, &[E1, E2, E3], &all, &[]),
            ReactionTapVerdict::CapReached
        );
        let kinds: Vec<String> = (0..11_u32)
            .map(|i| char::from_u32(0x1F630 + i).unwrap().to_string())
            .collect();
        assert_eq!(v(E5, &[], &all, &kinds), ReactionTapVerdict::KindLimit);
        assert_eq!(
            v(&kinds[0].clone(), &[], &all, &kinds),
            ReactionTapVerdict::Add
        );
    }

    // --- properties ----------------------------------------------------------

    mod properties {
        use super::*;
        use proptest::prelude::*;

        const PALETTE: [&str; 6] = [E1, E2, E3, E4, E5, "\u{1F622}"];

        fn person_ops() -> impl Strategy<Value = Vec<(bool, usize)>> {
            proptest::collection::vec((any::<bool>(), 0..PALETTE.len()), 0..12)
        }

        /// Interleave per-person op lists in the order `pick` says, giving
        /// each op a unique journal position.
        fn interleave(lists: &[Vec<(bool, usize)>], pick: &[usize]) -> Vec<ReactionOp> {
            let people = [A, B, C];
            let mut next = vec![0_usize; lists.len()];
            let mut out = Vec::new();
            let mut at = 0_i64;
            let mut picks = pick.iter().copied().cycle();
            while next.iter().zip(lists).any(|(n, l)| *n < l.len()) {
                let p = picks.next().unwrap_or(0) % lists.len();
                if next[p] >= lists[p].len() {
                    // Take the first list with ops left.
                    let Some(q) = (0..lists.len()).find(|&q| next[q] < lists[q].len()) else {
                        break;
                    };
                    let (is_add, e) = lists[q][next[q]];
                    next[q] += 1;
                    at += 1;
                    out.push(op(
                        people[q],
                        PALETTE[e],
                        if is_add {
                            ReactionChange::Add
                        } else {
                            ReactionChange::Remove
                        },
                        at,
                    ));
                    continue;
                }
                let (is_add, e) = lists[p][next[p]];
                next[p] += 1;
                at += 1;
                out.push(op(
                    people[p],
                    PALETTE[e],
                    if is_add {
                        ReactionChange::Add
                    } else {
                        ReactionChange::Remove
                    },
                    at,
                ));
            }
            out
        }

        proptest! {
            /// How distinct persons' ops interleave in the journal changes no
            /// person's result.
            #[test]
            fn the_interleaving_of_distinct_persons_does_not_matter(
                a in person_ops(),
                b in person_ops(),
                c in person_ops(),
                pick1 in proptest::collection::vec(0_usize..3, 1..40),
                pick2 in proptest::collection::vec(0_usize..3, 1..40),
            ) {
                let lists = [a, b, c];
                let one = fold_reactions(&interleave(&lists, &pick1));
                let two = fold_reactions(&interleave(&lists, &pick2));
                for person in [A, B, C] {
                    prop_assert_eq!(emojis_of(&one, person), emojis_of(&two, person));
                }
            }

            /// Nobody ever holds more than three, nor one emoji twice.
            #[test]
            fn the_cap_holds(
                a in person_ops(),
                b in person_ops(),
                pick in proptest::collection::vec(0_usize..2, 1..40),
            ) {
                let held = fold_reactions(&interleave(&[a, b], &pick));
                for person in [A, B] {
                    let mine = emojis_of(&held, person);
                    prop_assert!(mine.len() <= MAX_REACTIONS_PER_MESSAGE);
                    let mut unique = mine.clone();
                    unique.sort();
                    unique.dedup();
                    prop_assert_eq!(unique.len(), mine.len());
                }
            }

            /// Delivering the same ops again (a re-sync replay, each op at its
            /// own journal position) changes nothing.
            #[test]
            fn re_applying_the_same_ops_is_idempotent(
                a in person_ops(),
                b in person_ops(),
                pick in proptest::collection::vec(0_usize..2, 1..40),
            ) {
                let ops = interleave(&[a, b], &pick);
                let mut twice = ops.clone();
                twice.extend(ops.iter().cloned());
                prop_assert_eq!(fold_reactions(&twice), fold_reactions(&ops));
            }

            /// Every chip row is at most eleven chips, ordered by count.
            #[test]
            fn chip_rows_are_bounded_and_ordered(
                a in person_ops(),
                b in person_ops(),
                c in person_ops(),
                pick in proptest::collection::vec(0_usize..3, 1..40),
            ) {
                let held = fold_reactions(&interleave(&[a, b, c], &pick));
                let row = reaction_chips(
                    &chip_sources_from_held(&held, &A),
                    ChipConversation::Group,
                    &AllowedReactions::All,
                );
                prop_assert!(row.chips.len() <= MAX_REACTION_KINDS_SHOWN);
                for w in row.chips.windows(2) {
                    prop_assert!(w[0].count >= w[1].count);
                }
            }
        }
    }
}
