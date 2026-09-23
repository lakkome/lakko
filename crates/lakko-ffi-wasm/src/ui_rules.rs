// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Presentation rules the three clients must agree on — wasm-bindgen surface.
//!
//! Mirror of `lakko-ffi-uniffi/src/ui_rules.rs` over the same
//! [`lakko_ui_rules`] core. `docs/ux/navigation.md` requires that the rules
//! deciding what a person sees — the order of the one conversation list,
//! whether a typed string is worth a network lookup — exist ONCE rather than
//! as three translations of a paragraph.
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
use wasm_bindgen::prelude::*;

/// Which screen a conversation row opens.
///
/// Carried so a client can route a tap, and for nothing else. The list itself
/// says nothing about which kind a row is — no badge, no label, no differing
/// avatar — because a person who subscribed to a channel knows they subscribed
/// to a channel.
#[wasm_bindgen(js_name = ConversationKind)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationKind {
    /// A one-to-one conversation.
    Chat = 0,
    /// A group conversation.
    Group = 1,
    /// A channel this person subscribes to.
    Channel = 2,
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
/// Deliberately far smaller than the row the web client actually draws: the
/// order reads six fields and nothing else, so an avatar, a preview, an unread
/// count and a handle to open all stay on the JS side where they already are.
///
/// A constructible class rather than six parallel arrays, following
/// [`crate::contacts::BeaconObservation`] — the house shape for a small record
/// crossing this boundary. Parallel arrays would let the lengths diverge
/// silently and would need a sentinel for the absent timestamp that
/// `undefined` already expresses.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationRow {
    id: String,
    kind: ConversationKind,
    title: String,
    last_activity_ms: Option<i64>,
    pinned: bool,
    archived: bool,
}

#[wasm_bindgen]
impl ConversationRow {
    /// Build one row from what the client already holds.
    ///
    /// `lastActivityMs` is when the last thing a person can SEE happened, in
    /// unix milliseconds (BigInt on the JS side), or `undefined` for a
    /// conversation nothing has happened in yet. A row that draws nothing does
    /// not move a conversation to the top: a signal nobody sees is not
    /// activity.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(
        id: String,
        kind: ConversationKind,
        title: String,
        last_activity_ms: Option<i64>,
        pinned: bool,
        archived: bool,
    ) -> Self {
        Self {
            id,
            kind,
            title,
            last_activity_ms,
            pinned,
            archived,
        }
    }

    /// Stable within a kind; `(kind, id)` is unique in the list.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// Which screen the row opens.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> ConversationKind {
        self.kind
    }

    /// What the row is called. Read only to break a tie.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn title(&self) -> String {
        self.title.clone()
    }

    /// When the last thing a person can see happened, or `undefined`.
    #[wasm_bindgen(getter, js_name = lastActivityMs)]
    #[must_use]
    pub fn last_activity_ms(&self) -> Option<i64> {
        self.last_activity_ms
    }

    /// Pinned by this person.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn pinned(&self) -> bool {
        self.pinned
    }

    /// Archived by this person.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn archived(&self) -> bool {
        self.archived
    }
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
/// so the caller reorders the array it already holds instead of marshalling
/// every avatar and preview across this boundary twice to learn an order.
///
/// The order is total: two conversations nothing has happened in yet still
/// order the same way on every call, so an unrelated refresh never reshuffles
/// the list.
///
/// Each [`ConversationRow`] in the array is consumed by this call and must not
/// be used afterwards — build them at the call site from the rows you are
/// sorting, which is what the fresh-`lastActivityMs` reading requires anyway.
#[wasm_bindgen(js_name = orderConversations)]
#[must_use]
pub fn order_conversations(rows: Vec<ConversationRow>) -> Vec<u32> {
    let rust: Vec<RustConversationRow> = rows.into_iter().map(Into::into).collect();
    rust_order_conversations(&rust)
}

/// Strip the leading `@` people type and no namespace stores.
#[wasm_bindgen(js_name = bareName)]
#[must_use]
pub fn bare_name(typed: &str) -> String {
    rust_bare_name(typed)
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
#[wasm_bindgen(js_name = looksLikeAName)]
#[must_use]
pub fn looks_like_a_name(typed: &str) -> bool {
    rust_looks_like_a_name(typed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, kind: ConversationKind, at: Option<i64>) -> ConversationRow {
        ConversationRow::new(id.to_string(), kind, "Same".to_string(), at, false, false)
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
        assert!(looks_like_a_name("@anna"));
        assert!(!looks_like_a_name("ab"));
        assert_eq!(bare_name("  @anna "), "anna");
    }
}
