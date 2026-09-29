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

use lakko_ui_rules::call_bar::{
    call_bar_status as rust_call_bar_status, call_bar_visibility as rust_call_bar_visibility,
    CallBar as RustCallBar, CallBarPhase as RustCallBarPhase, CallBarStatus as RustCallBarStatus,
};
use lakko_ui_rules::capture::call_holds_capture as rust_call_holds_capture;
use lakko_ui_rules::conversation::{
    order_conversations as rust_order_conversations, ConversationKind as RustConversationKind,
    ConversationRow as RustConversationRow,
};
use lakko_ui_rules::folder::{
    folder_matches as rust_folder_matches,
    order_conversations_in_folder as rust_order_conversations_in_folder,
    FolderRule as RustFolderRule,
};
use lakko_ui_rules::info::{
    channel_info_sections as rust_channel_info_sections,
    group_info_sections as rust_group_info_sections, info_target as rust_info_target,
    person_info_sections as rust_person_info_sections,
    ChannelInfoCapabilities as RustChannelInfoCapabilities,
    GroupInfoCapabilities as RustGroupInfoCapabilities, InfoSection as RustInfoSection,
    InfoTarget as RustInfoTarget, PersonInfoCapabilities as RustPersonInfoCapabilities,
};
use lakko_ui_rules::lookup::{
    bare_name as rust_bare_name, looks_like_a_name as rust_looks_like_a_name,
};
use lakko_ui_rules::ring::{
    remaining_ring_ms as rust_remaining_ring_ms, ring_decision as rust_ring_decision,
    RingDecision as RustRingDecision, CALL_RING_WINDOW_MS,
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
#[allow(clippy::struct_excessive_bools)]
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
    /// Muted by this person. Read only by a folder's Muted exclude toggle
    /// (ADR-0058); the list order ignores it. Defaults to `false`, so
    /// existing initializer calls stay source-compatible.
    #[uniffi(default = false)]
    pub muted: bool,
    /// Something unread in it, or a manual "mark as unread" - what the
    /// unread badge means. Read only by a folder's Read exclude toggle.
    #[uniffi(default = false)]
    pub has_unread: bool,
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
            muted: r.muted,
            has_unread: r.has_unread,
        }
    }
}

/// A chat folder's rule in row terms (ADR-0058 section 6).
///
/// `include_kinds` / `exclude_flags` are the folder's meta-register bits as
/// the synced state returns them (include: bit0 one-to-one, bit1 groups,
/// bit2 channels; exclude: bit0 muted, bit1 read, bit2 archived).
/// `included` / `excluded` are the [`ConversationRow::id`]s the client maps
/// the folder's hand-added and hand-excluded references to.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FolderRule {
    /// Kind toggles.
    pub include_kinds: u32,
    /// Exclude toggles.
    pub exclude_flags: u32,
    /// Row ids added by hand.
    pub included: Vec<String>,
    /// Row ids excluded by hand.
    pub excluded: Vec<String>,
}

impl From<FolderRule> for RustFolderRule {
    fn from(r: FolderRule) -> Self {
        Self {
            include_kinds: r.include_kinds,
            exclude_flags: r.exclude_flags,
            included: r.included,
            excluded: r.excluded,
        }
    }
}

/// Whether `row` is in the folder: excluded by hand beats everything; then
/// included by kind toggle or by hand; then not muted / read / archived
/// while the matching exclude toggle is on (`docs/ux/chat-folders.md` 3.2).
#[uniffi::export]
#[must_use]
pub fn folder_matches(folder: FolderRule, row: ConversationRow) -> bool {
    rust_folder_matches(&folder.into(), &row.into())
}

/// Order a folder's rows: the folder's own pins first in their stored order
/// (`folder_pins` are row ids; unknown ids skipped), then the
/// [`order_conversations`] rule with the global pin ignored. Returns input
/// indices, every row exactly once.
#[uniffi::export]
#[must_use]
pub fn order_conversations_in_folder(
    rows: Vec<ConversationRow>,
    folder_pins: Vec<String>,
) -> Vec<u32> {
    let rust: Vec<RustConversationRow> = rows.into_iter().map(Into::into).collect();
    rust_order_conversations_in_folder(&rust, &folder_pins)
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

/// Which info a conversation's name opens — `docs/ux/conversation-info.md`
/// section 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum InfoTarget {
    /// A conversation between two people opens the other person.
    Person,
    /// A group opens the group.
    Group,
    /// A channel opens the channel.
    Channel,
}

impl From<RustInfoTarget> for InfoTarget {
    fn from(t: RustInfoTarget) -> Self {
        match t {
            RustInfoTarget::Person => Self::Person,
            RustInfoTarget::Group => Self::Group,
            RustInfoTarget::Channel => Self::Channel,
        }
    }
}

/// One block of an info layer — section 2.
///
/// The three targets share a shape and differ only in which blocks belong to
/// them. A client renders the blocks it is handed; it never decides which
/// ones those are, which is how the same paragraph came to be answered three
/// different ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum InfoSection {
    /// Open the conversation. Never offered from inside it.
    Message,
    /// Start a call on this conversation.
    Call,
    /// The out-of-band check between two people.
    SafetyNumber,
    /// The viewer's own name for a contact, which nobody else sees.
    RenameContact,
    /// Drop the contact from this device's list.
    RemoveContact,
    /// Forget a conversation this device is out of (ADR-0045 UX 6.2.4).
    DeleteChat,
    /// The shared name, shown to everyone (ADR-0051).
    GroupName,
    /// Change the shared name, for whoever may.
    RenameGroup,
    /// The roster, with each member's role where they have one.
    MemberList,
    /// Add somebody, for whoever may (ADR-0022).
    AddMember,
    /// Mint, copy and revoke an invite link (ADR-0028).
    InviteLink,
    /// Make admin and remove admin (ADR-0022).
    Roles,
    /// Hand the group to somebody else (ADR-0045).
    TransferOwnership,
    /// Leave the group.
    LeaveGroup,
    /// The `@handle` a channel is found by.
    ChannelHandle,
    /// What the channel says it is.
    ChannelDescription,
    /// How many devices read it. Only a publisher can know.
    SubscriberCount,
    /// The publisher's own screen.
    ManageChannel,
    /// Stop following a channel.
    Unsubscribe,
}

impl From<RustInfoSection> for InfoSection {
    fn from(s: RustInfoSection) -> Self {
        match s {
            RustInfoSection::Message => Self::Message,
            RustInfoSection::Call => Self::Call,
            RustInfoSection::SafetyNumber => Self::SafetyNumber,
            RustInfoSection::RenameContact => Self::RenameContact,
            RustInfoSection::RemoveContact => Self::RemoveContact,
            RustInfoSection::DeleteChat => Self::DeleteChat,
            RustInfoSection::GroupName => Self::GroupName,
            RustInfoSection::RenameGroup => Self::RenameGroup,
            RustInfoSection::MemberList => Self::MemberList,
            RustInfoSection::AddMember => Self::AddMember,
            RustInfoSection::InviteLink => Self::InviteLink,
            RustInfoSection::Roles => Self::Roles,
            RustInfoSection::TransferOwnership => Self::TransferOwnership,
            RustInfoSection::LeaveGroup => Self::LeaveGroup,
            RustInfoSection::ChannelHandle => Self::ChannelHandle,
            RustInfoSection::ChannelDescription => Self::ChannelDescription,
            RustInfoSection::SubscriberCount => Self::SubscriberCount,
            RustInfoSection::ManageChannel => Self::ManageChannel,
            RustInfoSection::Unsubscribe => Self::Unsubscribe,
        }
    }
}

/// What the viewer is to a one-to-one conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PersonInfoCapabilities {
    /// This device's membership in the conversation is over (ADR-0045 UX 6.2).
    /// A person's card is the contact card; there is no `is_contact`.
    pub membership_ended: bool,
}

impl From<PersonInfoCapabilities> for RustPersonInfoCapabilities {
    fn from(c: PersonInfoCapabilities) -> Self {
        Self {
            membership_ended: c.membership_ended,
        }
    }
}

/// What the viewer may do in a group. Every field is a decision the client
/// already made from the live role table; none is re-derived here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
#[allow(clippy::struct_excessive_bools)]
pub struct GroupInfoCapabilities {
    /// Owner or admin, and still a member (ADR-0022).
    pub can_manage_members: bool,
    /// Owner or admin, and still a member (ADR-0051).
    pub can_rename: bool,
    /// Owner or admin, and still a member (ADR-0028).
    pub can_mint_invite: bool,
    /// The owner, and still a member (ADR-0045).
    pub is_owner: bool,
    /// Whether this device can actually leave: ADR-0045's member exit is a
    /// `Leave` proposal, and a group with no ADR-0022 role table has no
    /// proposal path, so there is nothing for the control to do. Also `false`
    /// when this device cannot tell which leaf is its own.
    pub can_leave: bool,
    /// This device's membership is over: read-only (ADR-0045 UX 6.2).
    pub membership_ended: bool,
}

impl From<GroupInfoCapabilities> for RustGroupInfoCapabilities {
    fn from(c: GroupInfoCapabilities) -> Self {
        Self {
            can_manage_members: c.can_manage_members,
            can_rename: c.can_rename,
            can_mint_invite: c.can_mint_invite,
            is_owner: c.is_owner,
            can_leave: c.can_leave,
            membership_ended: c.membership_ended,
        }
    }
}

/// What the viewer is to a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct ChannelInfoCapabilities {
    /// This device holds the publisher keys.
    pub is_publisher: bool,
    /// This device reads the channel as a subscriber.
    pub is_subscriber: bool,
    /// This device has a subscriber count. Only a publisher ever does.
    pub subscriber_count_known: bool,
}

impl From<ChannelInfoCapabilities> for RustChannelInfoCapabilities {
    fn from(c: ChannelInfoCapabilities) -> Self {
        Self {
            is_publisher: c.is_publisher,
            is_subscriber: c.is_subscriber,
            subscriber_count_known: c.subscriber_count_known,
        }
    }
}

/// Which info the header opens for a conversation of this kind.
#[uniffi::export]
#[must_use]
pub fn info_target(kind: ConversationKind) -> InfoTarget {
    rust_info_target(kind.into()).into()
}

/// What a person's info holds — section 2.1.
///
/// Nothing in the result is ever group-shaped, whatever the capabilities:
/// there is no group to name, no owner to transfer, no admin to make, no
/// link to mint and nothing to leave but the person.
#[uniffi::export]
#[must_use]
pub fn person_info_sections(caps: PersonInfoCapabilities) -> Vec<InfoSection> {
    rust_person_info_sections(caps.into())
        .into_iter()
        .map(Into::into)
        .collect()
}

/// What a group's info holds — section 2.2.
#[uniffi::export]
#[must_use]
pub fn group_info_sections(caps: GroupInfoCapabilities) -> Vec<InfoSection> {
    rust_group_info_sections(caps.into())
        .into_iter()
        .map(Into::into)
        .collect()
}

/// What a channel's info holds — section 2.3.
#[uniffi::export]
#[must_use]
pub fn channel_info_sections(caps: ChannelInfoCapabilities) -> Vec<InfoSection> {
    rust_channel_info_sections(caps.into())
        .into_iter()
        .map(Into::into)
        .collect()
}

/// What an arriving call offer does to this device —
/// `docs/ux/calls.md` section 2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RingDecision {
    /// Ring: ringtone, vibration, and the call screen that takes over.
    Ring,
    /// Record the call, interrupt nobody. The conversation is muted.
    Silent,
    /// The caller has already given up. Record a missed call, show nothing.
    TooLate,
}

impl From<RustRingDecision> for RingDecision {
    fn from(d: RustRingDecision) -> Self {
        match d {
            RustRingDecision::Ring => Self::Ring,
            RustRingDecision::Silent => Self::Silent,
            RustRingDecision::TooLate => Self::TooLate,
        }
    }
}

/// Whether an arriving offer rings, given the conversation's mute flag and
/// how old the offer is.
///
/// `offerAgeMs` is measured from the delivery service's timestamp on the row,
/// never from a local clock: the two devices in a call share no clock, and
/// the window only works if both read it the same way.
#[uniffi::export]
#[must_use]
pub fn ring_decision(muted: bool, offer_age_ms: u64) -> RingDecision {
    rust_ring_decision(muted, offer_age_ms).into()
}

/// How much of the ring window is left, in milliseconds, for a platform
/// affordance that needs a deadline rather than a verdict. Saturates at zero.
#[uniffi::export]
#[must_use]
pub fn remaining_ring_ms(offer_age_ms: u64) -> u64 {
    rust_remaining_ring_ms(offer_age_ms)
}

/// How long a call rings before both sides give up, in milliseconds.
///
/// One copy, because it is a number two DIFFERENT devices must agree on: a
/// client carrying its own would leave one side ringing while the other had
/// already filed a missed call.
#[uniffi::export]
#[must_use]
pub fn call_ring_window_ms() -> u64 {
    CALL_RING_WINDOW_MS
}

/// Where a call is, as the collapsed bar of `docs/ux/calls.md` section 4.5
/// needs to see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallBarPhase {
    /// No call at all.
    Idle,
    /// An incoming call being offered, which nobody has answered.
    Ringing,
    /// Built, not yet on the wire.
    Preparing,
    /// Ringing out.
    Calling,
    /// Answered; the transport is coming up.
    Connecting,
    /// Media is flowing.
    Connected,
    /// The transport dropped and is being rebuilt.
    Reconnecting,
    /// Over.
    Ended,
}

impl From<CallBarPhase> for RustCallBarPhase {
    fn from(p: CallBarPhase) -> Self {
        match p {
            CallBarPhase::Idle => Self::Idle,
            CallBarPhase::Ringing => Self::Ringing,
            CallBarPhase::Preparing => Self::Preparing,
            CallBarPhase::Calling => Self::Calling,
            CallBarPhase::Connecting => Self::Connecting,
            CallBarPhase::Connected => Self::Connected,
            CallBarPhase::Reconnecting => Self::Reconnecting,
            CallBarPhase::Ended => Self::Ended,
        }
    }
}

/// What the collapsed bar's one status line says.
///
/// A sum type rather than three booleans, because `docs/ux/calls.md` section
/// 4 says the line carries the state or the running time and never two at
/// once - an invariant a view expressed in booleans is one careless branch
/// from breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallBarStatus {
    /// `Calling...`
    Calling,
    /// `Connecting...`
    Connecting,
    /// `Reconnecting...`
    Reconnecting,
    /// The running time, in milliseconds since the call connected. The client
    /// formats it; how long it has been is not a presentation decision.
    Elapsed {
        /// Milliseconds of connected media.
        ms: u64,
    },
}

/// Whether the collapsed call bar is on screen.
///
/// `callScreenUp` is the client's own read of its call presentation. One
/// state, one place: the bar exists because the call screen is away.
#[uniffi::export]
#[must_use]
pub fn call_bar_visible(phase: CallBarPhase, call_screen_up: bool) -> bool {
    matches!(
        rust_call_bar_visibility(phase.into(), call_screen_up),
        RustCallBar::Shown
    )
}

/// What the collapsed bar's one status line says.
///
/// `connectedForMs` is read only when the call is connected, so a caller with
/// no clock yet may pass anything.
#[uniffi::export]
#[must_use]
pub fn call_bar_status(phase: CallBarPhase, connected_for_ms: u64) -> CallBarStatus {
    match rust_call_bar_status(phase.into(), connected_for_ms) {
        RustCallBarStatus::Calling => CallBarStatus::Calling,
        RustCallBarStatus::Connecting => CallBarStatus::Connecting,
        RustCallBarStatus::Reconnecting => CallBarStatus::Reconnecting,
        RustCallBarStatus::Elapsed(ms) => CallBarStatus::Elapsed { ms },
    }
}

/// Whether the composer's voice and video-note recorder must stand aside
/// because a call is holding the microphone and camera.
///
/// `callIsLive` is the client's read of its one app-wide call slot: a call
/// collapsed into the bar of `docs/ux/calls.md` section 4.5 is live in every
/// chat, not only in the one it was placed from.
/// `unansweredIncomingRing` narrows it to the one state that owns no
/// capture: an incoming call still being offered. An outgoing call holds
/// capture from the moment it is placed, so it is deliberately not carved
/// out.
#[uniffi::export]
#[must_use]
pub fn call_holds_capture(call_is_live: bool, unanswered_incoming_ring: bool) -> bool {
    rust_call_holds_capture(call_is_live, unanswered_incoming_ring)
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
            muted: false,
            has_unread: false,
        }
    }

    #[test]
    fn the_folder_rule_crosses_the_boundary() {
        let rule = FolderRule {
            include_kinds: 0b100,
            exclude_flags: 0b010,
            included: vec!["g".into()],
            excluded: vec!["c2".into()],
        };
        let mut unread = row("c1", ConversationKind::Channel, Some(1));
        unread.has_unread = true;
        assert!(folder_matches(rule.clone(), unread.clone()));
        // Read is excluded while the Read toggle is on.
        assert!(!folder_matches(
            rule.clone(),
            row("c1", ConversationKind::Channel, Some(1))
        ));
        let mut hand = row("g", ConversationKind::Group, Some(1));
        hand.has_unread = true;
        assert!(folder_matches(rule.clone(), hand));
        let mut excluded = row("c2", ConversationKind::Channel, Some(1));
        excluded.has_unread = true;
        assert!(!folder_matches(rule, excluded));

        let rows = vec![
            row("a", ConversationKind::Chat, Some(1)),
            row("b", ConversationKind::Chat, Some(2)),
        ];
        assert_eq!(
            order_conversations_in_folder(rows, vec!["a".into()]),
            vec![0, 1]
        );
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
    fn the_sections_cross_the_boundary_as_the_core_decided_them() {
        let ended = person_info_sections(PersonInfoCapabilities {
            membership_ended: true,
        });
        assert!(ended.contains(&InfoSection::DeleteChat));
        assert!(!ended.contains(&InfoSection::Call));
        assert!(channel_info_sections(ChannelInfoCapabilities {
            is_publisher: false,
            is_subscriber: true,
            subscriber_count_known: false,
        })
        .contains(&InfoSection::Unsubscribe));
        assert_eq!(info_target(ConversationKind::Chat), InfoTarget::Person);
    }

    #[test]
    fn the_ring_rule_crosses_the_boundary_intact() {
        assert_eq!(ring_decision(false, 0), RingDecision::Ring);
        assert_eq!(ring_decision(true, 0), RingDecision::Silent);
        assert_eq!(
            ring_decision(false, call_ring_window_ms()),
            RingDecision::TooLate
        );
        assert_eq!(remaining_ring_ms(call_ring_window_ms()), 0);
    }

    #[test]
    fn the_gate_agrees_with_the_core_across_the_boundary() {
        assert!(looks_like_a_name("@anna".to_string()));
        assert!(!looks_like_a_name("ab".to_string()));
        assert_eq!(bare_name("  @anna ".to_string()), "anna");
    }
}
