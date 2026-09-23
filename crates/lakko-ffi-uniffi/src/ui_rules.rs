// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Presentation rules the three clients must agree on — UniFFI surface.
//!
//! Mirror of `lakko-ffi-wasm/src/ui_rules.rs` over the same
//! [`lakko_ui_rules`] core. `docs/ux/navigation.md` requires that the rules
//! deciding what a person sees — the order of the one conversation list,
//! whether a typed string is worth a network lookup — exist ONCE rather than
//! as three translations of a paragraph. This is two of the three places that
//! one copy is reachable from.
//!
//! Nothing here touches a screen, a store or the network. A client maps what
//! it already holds into these types, asks, and renders the answer.

use lakko_ui_rules::conversation::{
    order_conversations as rust_order_conversations, ConversationKind as RustConversationKind,
    ConversationRow as RustConversationRow,
};
use lakko_ui_rules::lookup::{
    bare_name as rust_bare_name, looks_like_a_name as rust_looks_like_a_name,
};

/// Which screen a conversation row opens.
///
/// Carried so a client can route a tap, and for nothing else. The list itself
/// says nothing about which kind a row is — no badge, no label, no differing
/// avatar — because a person who subscribed to a channel knows they subscribed
/// to a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ConversationKind {
    /// A one-to-one conversation.
    Chat,
    /// A group conversation.
    Group,
    /// A channel this person subscribes to.
    Channel,
}

impl From<ConversationKind> for RustConversationKind {
    fn from(k: ConversationKind) -> Self {
        match k {
            ConversationKind::Chat => Self::Chat,
            ConversationKind::Group => Self::Group,
            ConversationKind::Channel => Self::Channel,
        }
    }
}

/// One row of the merged list, reduced to what the order reads.
///
/// Deliberately far smaller than the row a client actually draws: the order
/// reads six fields and nothing else, so an avatar, a preview, an unread count
/// and a handle to open all stay on the client side where they already are.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConversationRow {
    /// Stable within a kind; `(kind, id)` is unique in the list.
    pub id: String,
    /// Which screen the row opens.
    pub kind: ConversationKind,
    /// What the row is called. Read only to break a tie.
    pub title: String,
    /// When the last thing a person can SEE happened, in unix milliseconds, or
    /// `None` for a conversation nothing has happened in yet.
    ///
    /// A row that draws nothing does not move a conversation to the top: a
    /// signal nobody sees is not activity.
    pub last_activity_ms: Option<i64>,
    /// Pinned by this person. Beats everything, including something newer.
    pub pinned: bool,
    /// Archived by this person. Sinks below everything, including something
    /// newer.
    pub archived: bool,
}

impl From<ConversationRow> for RustConversationRow {
    fn from(r: ConversationRow) -> Self {
        Self {
            id: r.id,
            kind: r.kind.into(),
            title: r.title,
            last_activity_ms: r.last_activity_ms,
            pinned: r.pinned,
            archived: r.archived,
        }
    }
}

/// Order the one conversation list: pinned first, archived last, newest
/// between.
///
/// Returns the input INDICES in display order rather than the rows themselves,
/// so a client reorders what it already holds instead of marshalling every
/// avatar and preview across this boundary twice to learn an order.
///
/// The order is total: two conversations nothing has happened in yet still
/// order the same way on every call, so an unrelated refresh never reshuffles
/// the list.
#[uniffi::export]
#[must_use]
pub fn order_conversations(rows: Vec<ConversationRow>) -> Vec<u32> {
    let rust: Vec<RustConversationRow> = rows.into_iter().map(Into::into).collect();
    rust_order_conversations(&rust)
}

/// Strip the leading `@` people type and no namespace stores.
#[uniffi::export]
#[must_use]
pub fn bare_name(typed: String) -> String {
    rust_bare_name(&typed)
}

/// Whether a typed string is worth spending a network lookup on.
///
/// The predicate is the canonical username rule the discovery blinder itself
/// enforces, not a copy of it: asking here is the difference between a quiet
/// "type the whole name" and making a person pay a proof of work to be told
/// no.
///
/// It is NOT a check that the name exists — it cannot be. The server evaluates
/// a blinded name without learning it, so it answers only "does this exact
/// name exist" and nothing about prefixes. A client must offer this as "look
/// up the whole name", never as a list of partial matches.
#[uniffi::export]
#[must_use]
pub fn looks_like_a_name(typed: String) -> bool {
    rust_looks_like_a_name(&typed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, kind: ConversationKind, at: Option<i64>) -> ConversationRow {
        ConversationRow {
            id: id.to_string(),
            kind,
            title: "Same".to_string(),
            last_activity_ms: at,
            pinned: false,
            archived: false,
        }
    }

    #[test]
    fn the_boundary_carries_the_order_the_core_decided() {
        let rows = vec![
            row("old", ConversationKind::Chat, Some(1)),
            row("new", ConversationKind::Channel, Some(9)),
        ];
        assert_eq!(order_conversations(rows), vec![1, 0]);
    }

    #[test]
    fn the_gate_agrees_with_the_core_across_the_boundary() {
        assert!(looks_like_a_name("@anna".to_string()));
        assert!(!looks_like_a_name("ab".to_string()));
        assert_eq!(bare_name("  @anna ".to_string()), "anna");
    }
}
