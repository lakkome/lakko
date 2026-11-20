// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! What a block does on each path (ADR-0098 section 6, UX
//! `reporting-and-moderation.md` section 1.2).
//!
//! A block is a register the account's devices share; every receive path
//! asks this rule what to do with something from a blocked person, so the
//! four clients cannot each decide differently. The client resolves the
//! sender to an account key at receipt (C-68) and reads the register; this
//! rule only maps the path to the verdict.

/// Where something from (or to) a person meets the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockPath {
    /// A message, edit, delete or reaction in the one-to-one with them.
    OneToOneMessage,
    /// A message of theirs in a group both are in.
    GroupMessage,
    /// A contact request from them, whatever route it came by (inbox,
    /// invite link, through a group).
    ContactRequest,
    /// A call from them ringing on this device: a one-to-one offer, or a
    /// group call they started.
    IncomingCall,
    /// Their tile in a call both are in.
    CallTile,
    /// A system notification, a badge or a push preview for something of
    /// theirs.
    Notification,
    /// Typing, a read receipt or a profile update this device would send to
    /// them in the one-to-one.
    OutgoingSignal,
    /// Their typing indicator, read receipt or profile update arriving in
    /// the one-to-one.
    IncomingSignal,
}

/// What the client does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockVerdict {
    /// As if there were no block.
    Show,
    /// Not shown, not stored, not notified. Nothing tells the sender.
    Drop,
    /// One muted row, "Message from someone you blocked", that shows the
    /// message when tapped.
    Collapse,
    /// The call is declined without a ring and leaves no missed-call row.
    DeclineSilently,
    /// Shown with a muted "Blocked" label.
    Label,
    /// Not sent.
    Withhold,
}

/// The verdict for `path` when the person is (`blocked`) or is not blocked.
///
/// Total and cheap; a client calls it on every item rather than caching.
#[must_use]
pub const fn block_verdict(path: BlockPath, blocked: bool) -> BlockVerdict {
    if !blocked {
        return BlockVerdict::Show;
    }
    match path {
        BlockPath::OneToOneMessage
        | BlockPath::ContactRequest
        | BlockPath::Notification
        | BlockPath::IncomingSignal => BlockVerdict::Drop,
        BlockPath::GroupMessage => BlockVerdict::Collapse,
        BlockPath::IncomingCall => BlockVerdict::DeclineSilently,
        BlockPath::CallTile => BlockVerdict::Label,
        BlockPath::OutgoingSignal => BlockVerdict::Withhold,
    }
}

impl BlockPath {
    /// The path's name in kebab case, as the wasm surface passes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::OneToOneMessage => "one-to-one-message",
            Self::GroupMessage => "group-message",
            Self::ContactRequest => "contact-request",
            Self::IncomingCall => "incoming-call",
            Self::CallTile => "call-tile",
            Self::Notification => "notification",
            Self::OutgoingSignal => "outgoing-signal",
            Self::IncomingSignal => "incoming-signal",
        }
    }

    /// Every path, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::OneToOneMessage,
        Self::GroupMessage,
        Self::ContactRequest,
        Self::IncomingCall,
        Self::CallTile,
        Self::Notification,
        Self::OutgoingSignal,
        Self::IncomingSignal,
    ];

    /// Parse a name written by [`Self::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

impl BlockVerdict {
    /// The verdict's name in kebab case, as the wasm surface returns it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::Drop => "drop",
            Self::Collapse => "collapse",
            Self::DeclineSilently => "decline-silently",
            Self::Label => "label",
            Self::Withhold => "withhold",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_changes_for_a_person_who_is_not_blocked() {
        for p in BlockPath::ALL {
            assert_eq!(block_verdict(p, false), BlockVerdict::Show);
        }
    }

    #[test]
    fn each_path_has_the_verdict_the_spec_gives_it() {
        use BlockPath as P;
        use BlockVerdict as V;
        let table = [
            (P::OneToOneMessage, V::Drop),
            (P::GroupMessage, V::Collapse),
            (P::ContactRequest, V::Drop),
            (P::IncomingCall, V::DeclineSilently),
            (P::CallTile, V::Label),
            (P::Notification, V::Drop),
            (P::OutgoingSignal, V::Withhold),
            (P::IncomingSignal, V::Drop),
        ];
        assert_eq!(table.len(), P::ALL.len());
        for (p, v) in table {
            assert_eq!(block_verdict(p, true), v, "{p:?}");
        }
    }

    #[test]
    fn names_round_trip() {
        for p in BlockPath::ALL {
            assert_eq!(BlockPath::from_name(p.name()), Some(p));
        }
        assert_eq!(BlockPath::from_name("group"), None);
    }
}
