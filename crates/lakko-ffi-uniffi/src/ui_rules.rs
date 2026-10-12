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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::scoped_discovery::{FfiGroupsForm, FfiScopeId, FfiVisibilityMode, FfiVisibilitySetting};
use crate::FfiError;
use lakko_contacts::visibility::{ScopeId as RustScopeId, VisibilitySetting as RustSetting};
use lakko_ui_rules::av_align::{
    av_align_capability as rust_av_align_capability, AvAlign as RustAvAlign,
    AvAudioPlan as RustAvAudioPlan, AvAudioReading as RustAvAudioReading,
    AvAudioReason as RustAvAudioReason, AvCapability as RustAvCapability, AvPlan as RustAvPlan,
    AvReadings as RustAvReadings, AvVideoPlan as RustAvVideoPlan,
    AvVideoReading as RustAvVideoReading, AvVideoReason as RustAvVideoReason, AV_DEAD_BAND_MS,
    AV_READING_MS, AV_SOUND_WAIT_MAX_MS,
};
use lakko_ui_rules::call_bar::{
    call_bar_status as rust_call_bar_status, call_bar_visibility as rust_call_bar_visibility,
    CallBar as RustCallBar, CallBarPhase as RustCallBarPhase, CallBarStatus as RustCallBarStatus,
};
use lakko_ui_rules::call_group::{
    call_grid as rust_call_grid, call_stage as rust_call_stage,
    middle_sender_wanted as rust_middle_sender_wanted, ringback_plays as rust_ringback_plays,
    tile_video_quality as rust_tile_video_quality, StageCandidate as RustStageCandidate,
    TileVideoQuality as RustTileVideoQuality, LIGHT_MAX_WIDTH_PX, MAIN_MIN_WIDTH_PX,
    MIDDLE_SENDER_MIN_PEOPLE,
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
use lakko_ui_rules::layer_display::{
    LayerDisplay as RustLayerDisplay, LayerDisplayCounters as RustLayerDisplayCounters,
    ShownLayer as RustShownLayer, FIRST_FRAME_GRACE_MS, FULL_STALL_MS, FULL_STEADY_MS,
    LAYER_TICK_MS, LIGHT_FRESH_MS,
};
use lakko_ui_rules::lookup::{
    bare_name as rust_bare_name, looks_like_a_name as rust_looks_like_a_name,
};

use lakko_ui_rules::ring::{
    remaining_ring_ms as rust_remaining_ring_ms, ring_decision as rust_ring_decision,
    RingDecision as RustRingDecision, CALL_RING_WINDOW_MS,
};
use lakko_ui_rules::speaking::{
    SpeakingTracker as RustSpeakingTracker, FLOOR_ONSET_MS, RING_OFF_MS, RING_ON_MS,
    SPEAKING_LEVEL, SPEAKING_LEVEL_MAX, SPEAKING_LEVEL_MIN,
};
use lakko_ui_rules::visibility::{
    choose_mode as rust_choose_mode, exclude_scope_under_all as rust_exclude_scope_under_all,
    set_contacts_of_contacts as rust_set_contacts_of_contacts, set_groups as rust_set_groups,
    set_groups_form as rust_set_groups_form, set_scope_chosen as rust_set_scope_chosen,
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
    /// The channel's member list, for its readers (ADR-0060 section 3,
    /// UX `who-can-find-me.md` 6.3). Appended last: uniffi decodes by
    /// ordinal.
    ChannelMembers,
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
            RustInfoSection::ChannelMembers => Self::ChannelMembers,
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

// ============================================================================
// "Who can find me" — the Settings transitions (UX section 11, ADR-0060 §7)
// ============================================================================
//
// Each takes the current setting and returns the next one; none stamps
// `updated_at_ms` (the caller does, then composes the envelope). Every result
// satisfies the setting's rule, so a client never has to decide what a tap on
// the last part means.

/// Choose one of the three positions. Leaving Some people remembers its
/// parts; coming back restores them (or the default parts when none were
/// remembered).
#[uniffi::export]
#[must_use]
pub fn choose_visibility_mode(
    setting: FfiVisibilitySetting,
    mode: FfiVisibilityMode,
) -> FfiVisibilitySetting {
    rust_choose_mode(&setting.into(), mode.into()).into()
}

/// Switch part 2.1 (contacts of my contacts). Turning the last part off
/// moves to Nobody; turning a part on from Nobody or Everyone moves to Some
/// people.
#[uniffi::export]
#[must_use]
pub fn set_contacts_of_contacts(setting: FfiVisibilitySetting, on: bool) -> FfiVisibilitySetting {
    rust_set_contacts_of_contacts(&setting.into(), on).into()
}

/// Switch part 2.2 (groups and channels), keeping its form. Same position
/// rules as [`set_contacts_of_contacts`].
#[uniffi::export]
#[must_use]
pub fn set_groups_visible(setting: FfiVisibilitySetting, on: bool) -> FfiVisibilitySetting {
    rust_set_groups(&setting.into(), on).into()
}

/// All groups and channels, or only the chosen ones. No effect unless part
/// 2.2 is on.
#[uniffi::export]
#[must_use]
pub fn set_groups_form(setting: FfiVisibilitySetting, form: FfiGroupsForm) -> FfiVisibilitySetting {
    rust_set_groups_form(&setting.into(), form.into()).into()
}

/// Exclude one group or channel while "all" is on: the form becomes "only
/// the ones I choose" with every other scope in `all_scopes` chosen.
///
/// # Errors
///
/// [`FfiError::Visibility`] `scopeIdLength` / `tooManyChosen`.
#[uniffi::export]
pub fn exclude_scope_under_all(
    setting: FfiVisibilitySetting,
    all_scopes: Vec<FfiScopeId>,
    excluded: FfiScopeId,
) -> Result<FfiVisibilitySetting, FfiError> {
    let all: Vec<RustScopeId> = all_scopes.into_iter().map(Into::into).collect();
    let s: RustSetting = setting.into();
    Ok(rust_exclude_scope_under_all(&s, &all, &excluded.into())?.into())
}

/// Choose or unchoose one group or channel in the chosen list.
///
/// # Errors
///
/// [`FfiError::Visibility`] `scopeIdLength` / `tooManyChosen`.
#[uniffi::export]
pub fn set_scope_chosen(
    setting: FfiVisibilitySetting,
    scope: FfiScopeId,
    on: bool,
) -> Result<FfiVisibilitySetting, FfiError> {
    Ok(rust_set_scope_chosen(&setting.into(), &scope.into(), on)?.into())
}

/// Whether the outgoing tone loops (`docs/ux/calls.md` section 3.4).
///
/// Only while a one-to-one call this device placed waits for an answer. A
/// group call, or a call joined from a link, rings nobody.
#[uniffi::export]
#[must_use]
pub fn ringback_plays(group_call: bool, phase: CallBarPhase) -> bool {
    rust_ringback_plays(group_call, phase.into())
}

/// How a group call's grid is drawn (`docs/ux/group-call-grid.md` section 4):
/// equal tiles filling the call area, with no scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct CallGrid {
    /// Columns on a page.
    pub cols: u32,
    /// Rows on a full page.
    pub rows: u32,
    /// Width of every tile.
    pub tile_width: u32,
    /// Height of every tile.
    pub tile_height: u32,
    /// The space between tiles.
    pub gap: u32,
    /// Tiles on a full page.
    pub per_page: u32,
    /// Pages; 1 when everyone fits.
    pub pages: u32,
}

/// Lay `count` equal tiles into a call area `width` by `height` points (the
/// space between the status line and the control bar, inside the side
/// margins).
#[uniffi::export]
#[must_use]
pub fn call_grid(count: u32, width: u32, height: u32) -> CallGrid {
    let grid = rust_call_grid(count, width, height);
    CallGrid {
        cols: grid.cols,
        rows: grid.rows,
        tile_width: grid.tile_width,
        tile_height: grid.tile_height,
        gap: grid.gap,
        per_page: grid.per_page,
        pages: grid.pages,
    }
}

/// One person, as the stage rule sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct StageCandidate {
    /// This device's own tile.
    pub is_local: bool,
    /// The person is sending a picture.
    pub camera_on: bool,
    /// The person is the active speaker right now.
    pub speaking: bool,
    /// When the person last was the active speaker, in milliseconds on the
    /// caller's own clock; `0` for never.
    pub last_spoke_at_ms: u64,
}

/// Who is on the stage in the layout with one large tile: an index into
/// `people`, or `None` when the call is empty.
#[uniffi::export]
#[must_use]
pub fn call_stage(people: Vec<StageCandidate>, pinned: Option<u32>) -> Option<u32> {
    let people: Vec<RustStageCandidate> = people
        .into_iter()
        .map(|p| RustStageCandidate {
            is_local: p.is_local,
            camera_on: p.camera_on,
            speaking: p.speaking,
            last_spoke_at_ms: p.last_spoke_at_ms,
        })
        .collect();
    let pinned = pinned.and_then(|p| usize::try_from(p).ok());
    rust_call_stage(&people, pinned).and_then(|index| u32::try_from(index).ok())
}

/// Which of a person's video tracks a call tile asks the call server for
/// (`docs/ux/group-call-grid.md` section 7.6, ADR-0070 step 5). The light
/// track always arrives beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum TileVideoQuality {
    /// The main track: the full picture.
    Main,
    /// The middle track: the camera at half size.
    Middle,
    /// The light track alone.
    Light,
}

impl From<RustTileVideoQuality> for TileVideoQuality {
    fn from(quality: RustTileVideoQuality) -> Self {
        match quality {
            RustTileVideoQuality::Main => Self::Main,
            RustTileVideoQuality::Middle => Self::Middle,
            RustTileVideoQuality::Light => Self::Light,
        }
    }
}

/// Which track a tile asks for. `people` is everyone in the call, this
/// device's own person included; `tile_width_px` is the width the tile is
/// drawn at in **physical** pixels (points times the display scale);
/// `pinned_or_stage` says the tile is the pinned person or the stage.
///
/// A call of two: `Main`. A pinned or stage tile at least
/// [`tile_main_min_width_px`] wide: `Main`. Otherwise a tile narrower than
/// [`tile_light_max_width_px`]: `Light`. Otherwise: `Middle`.
#[uniffi::export]
#[must_use]
pub fn tile_video_quality(
    people: u32,
    tile_width_px: u32,
    pinned_or_stage: bool,
) -> TileVideoQuality {
    rust_tile_video_quality(people, tile_width_px, pinned_or_stage).into()
}

/// Whether this device sends the middle copy of its camera: only from
/// [`middle_sender_min_people`] people in the call on (its own person
/// included).
#[uniffi::export]
#[must_use]
pub fn middle_sender_wanted(people: u32) -> bool {
    rust_middle_sender_wanted(people)
}

/// A pinned or stage tile at least this wide, in physical pixels, asks for
/// the main track. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn tile_main_min_width_px() -> u32 {
    MAIN_MIN_WIDTH_PX
}

/// A tile narrower than this, in physical pixels, asks for the light track
/// alone. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn tile_light_max_width_px() -> u32 {
    LIGHT_MAX_WIDTH_PX
}

/// A person sends the middle copy of their camera from this many people in
/// the call on. Chosen.
#[uniffi::export]
#[must_use]
pub fn middle_sender_min_people() -> u32 {
    MIDDLE_SENDER_MIN_PEOPLE
}

/// Which of a remote person's two video tracks a call tile shows
/// (`docs/ux/group-call-grid.md` section 7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ShownLayer {
    /// The person's main picture.
    Full,
    /// The light companion.
    Light,
}

impl From<RustShownLayer> for ShownLayer {
    fn from(layer: RustShownLayer) -> Self {
        match layer {
            RustShownLayer::Full => Self::Full,
            RustShownLayer::Light => Self::Light,
        }
    }
}

/// What a [`LayerDisplay`] counted; milliseconds where it says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct LayerDisplayCounters {
    /// Times the tile switched to the light track.
    pub light_switches: u64,
    /// Total time the light track was the shown one, milliseconds.
    pub light_ms: u64,
    /// The longest displayed stall, milliseconds.
    pub stall_max_ms: u64,
    /// Total displayed stall time, milliseconds.
    pub stall_total_ms: u64,
}

impl From<RustLayerDisplayCounters> for LayerDisplayCounters {
    fn from(c: RustLayerDisplayCounters) -> Self {
        Self {
            light_switches: c.light_switches,
            light_ms: c.light_ms,
            stall_max_ms: c.stall_max_ms,
            stall_total_ms: c.stall_total_ms,
        }
    }
}

/// One remote tile's choice between the person's main video track and its
/// light companion: the tile never stands still while a lighter copy of the
/// picture is arriving.
///
/// One per remote tile. The tile reports every presented frame of each track
/// and calls [`LayerDisplay::tick`] every [`layer_tick_ms`], all on one clock
/// in whole milliseconds (any origin; a timestamp earlier than the latest one
/// seen is read as the latest), and shows what comes back. Thread-safe: a
/// frame callback on a render thread and a tick on the main thread may call
/// in at once.
#[derive(uniffi::Object)]
pub struct LayerDisplay {
    inner: Mutex<RustLayerDisplay>,
}

impl LayerDisplay {
    /// The state is plain numbers and no method can panic while holding the
    /// lock, so a poisoned lock still guards a consistent state.
    fn state(&self) -> MutexGuard<'_, RustLayerDisplay> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[uniffi::export]
impl LayerDisplay {
    /// A tile that started showing this person at `now_ms`; `light_available`
    /// is whether the person has a light track yet.
    #[uniffi::constructor]
    #[must_use]
    pub fn new(now_ms: u64, light_available: bool) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RustLayerDisplay::new(now_ms, light_available)),
        })
    }

    /// The full track presented a frame at `now_ms`.
    pub fn full_frame(&self, now_ms: u64) -> ShownLayer {
        self.state().full_frame(now_ms).into()
    }

    /// The light track presented a frame at `now_ms`.
    pub fn light_frame(&self, now_ms: u64) -> ShownLayer {
        self.state().light_frame(now_ms).into()
    }

    /// Time has passed with no frame: notices a stall without a frame
    /// arriving.
    pub fn tick(&self, now_ms: u64) -> ShownLayer {
        self.state().tick(now_ms).into()
    }

    /// The person's light track appeared or went away. Without one the tile
    /// shows the full track, whatever it is doing.
    pub fn set_light_available(&self, available: bool, now_ms: u64) -> ShownLayer {
        self.state().set_light_available(available, now_ms).into()
    }

    /// The tile could not observe frames until `now_ms` (the app was in the
    /// background). Nothing is counted for that time and the tile starts
    /// again on the full track, as a new tile would.
    pub fn resync(&self, now_ms: u64) -> ShownLayer {
        self.state().resync(now_ms).into()
    }

    /// What the tile shows.
    #[must_use]
    pub fn shown(&self) -> ShownLayer {
        self.state().shown().into()
    }

    /// Everything counted since the tile started.
    #[must_use]
    pub fn counters(&self) -> LayerDisplayCounters {
        self.state().counters().into()
    }

    /// Everything counted since the last drain, and start a new interval. A
    /// displayed stall still in progress is reported in each interval it runs
    /// through.
    pub fn drain(&self) -> LayerDisplayCounters {
        self.state().drain().into()
    }
}

/// Two sets of layer-display counters as one: sums, and the longer of the
/// longest stalls. For adding up the tiles that drew one person.
#[uniffi::export]
#[must_use]
pub fn add_layer_display_counters(
    a: LayerDisplayCounters,
    b: LayerDisplayCounters,
) -> LayerDisplayCounters {
    let rust = |c: LayerDisplayCounters| RustLayerDisplayCounters {
        light_switches: c.light_switches,
        light_ms: c.light_ms,
        stall_max_ms: c.stall_max_ms,
        stall_total_ms: c.stall_total_ms,
    };
    rust(a).plus(rust(b)).into()
}

/// How often a tile calls [`LayerDisplay::tick`], milliseconds. Chosen, not
/// measured: a third of [`layer_full_stall_ms`].
#[uniffi::export]
#[must_use]
pub fn layer_tick_ms() -> u64 {
    LAYER_TICK_MS
}

/// The full track is standing still once it has presented no frame for this
/// long, milliseconds. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn layer_full_stall_ms() -> u64 {
    FULL_STALL_MS
}

/// The light track is playing if it presented a frame within this,
/// milliseconds. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn layer_light_fresh_ms() -> u64 {
    LIGHT_FRESH_MS
}

/// How long the full track must present frames without a gap before the tile
/// returns to it, milliseconds. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn layer_full_steady_ms() -> u64 {
    FULL_STEADY_MS
}

/// How long a full track that has never presented a frame is waited for
/// before a playing light track is shown instead, milliseconds. Chosen, not
/// measured.
#[uniffi::export]
#[must_use]
pub fn layer_first_frame_grace_ms() -> u64 {
    FIRST_FRAME_GRACE_MS
}

// ----- sound and picture together (ADR-0070, note of 2026-10-12) -----------

/// What the audio receiver of one remote person reports for one interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AvAudioReading {
    /// Packets arrived in the interval.
    pub flowing: bool,
    /// Average time a sample spent in the jitter buffer over the interval,
    /// milliseconds; `None` when nothing left the buffer or the platform
    /// reports none.
    pub delay_ms: Option<u32>,
    /// The delay the network alone would need, milliseconds, where the
    /// platform reports it.
    pub network_ms: Option<u32>,
}

/// What one video receiver of one remote person reports for one interval.
/// A track the person does not have, or one the SFU has paused, is
/// `flowing: false` with everything else empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AvVideoReading {
    /// Packets arrived in the interval.
    pub flowing: bool,
    /// A tile is drawing this track now. Only a shown track can make the
    /// sound wait.
    pub shown: bool,
    /// The media engine synchronises this track with the audio itself: the
    /// main track, which shares the audio's stream id.
    pub engine_synced: bool,
    /// Average time a frame spent in the jitter buffer over the interval,
    /// milliseconds; `None` when no frame left it or the platform reports
    /// none.
    pub delay_ms: Option<u32>,
    /// The delay the network alone would need, milliseconds, where the
    /// platform reports it.
    pub network_ms: Option<u32>,
}

impl From<AvVideoReading> for RustAvVideoReading {
    fn from(r: AvVideoReading) -> Self {
        Self {
            flowing: r.flowing,
            shown: r.shown,
            engine_synced: r.engine_synced,
            delay_ms: r.delay_ms,
            network_ms: r.network_ms,
        }
    }
}

/// One interval's readings for one remote person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AvReadings {
    /// The audio receiver.
    pub audio: AvAudioReading,
    /// The main video track (stream id `<peer>`).
    pub main: AvVideoReading,
    /// The middle video track (`<peer>~mid`).
    pub middle: AvVideoReading,
    /// The light video track (`<peer>~light`).
    pub light: AvVideoReading,
}

impl From<AvReadings> for RustAvReadings {
    fn from(r: AvReadings) -> Self {
        Self {
            audio: RustAvAudioReading {
                flowing: r.audio.flowing,
                delay_ms: r.audio.delay_ms,
                network_ms: r.audio.network_ms,
            },
            main: r.main.into(),
            middle: r.middle.into(),
            light: r.light.into(),
        }
    }
}

/// Why the audio receiver's target is what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AvAudioReason {
    /// No audio packets arrived; nothing is changed.
    NotFlowing,
    /// The audio flows but reported no delay; nothing is changed.
    NoReading,
    /// This device cannot set an audio receiver's delay.
    NoSetter,
    /// Nothing is asked of the audio.
    Free,
    /// The sound waits for a picture that needs more delay than it has.
    WaitingForPicture,
    /// The sound waits as long as it may; the picture needs more and is
    /// left late.
    WaitBounded,
    /// No picture needs the sound to wait any more; the target is coming
    /// down.
    Releasing,
    /// The engine does not follow the audio target; it is no longer
    /// corrected.
    HintIgnored,
}

impl From<RustAvAudioReason> for AvAudioReason {
    fn from(r: RustAvAudioReason) -> Self {
        match r {
            RustAvAudioReason::NotFlowing => Self::NotFlowing,
            RustAvAudioReason::NoReading => Self::NoReading,
            RustAvAudioReason::NoSetter => Self::NoSetter,
            RustAvAudioReason::Free => Self::Free,
            RustAvAudioReason::WaitingForPicture => Self::WaitingForPicture,
            RustAvAudioReason::WaitBounded => Self::WaitBounded,
            RustAvAudioReason::Releasing => Self::Releasing,
            RustAvAudioReason::HintIgnored => Self::HintIgnored,
        }
    }
}

impl From<AvAudioReason> for RustAvAudioReason {
    fn from(r: AvAudioReason) -> Self {
        match r {
            AvAudioReason::NotFlowing => Self::NotFlowing,
            AvAudioReason::NoReading => Self::NoReading,
            AvAudioReason::NoSetter => Self::NoSetter,
            AvAudioReason::Free => Self::Free,
            AvAudioReason::WaitingForPicture => Self::WaitingForPicture,
            AvAudioReason::WaitBounded => Self::WaitBounded,
            AvAudioReason::Releasing => Self::Releasing,
            AvAudioReason::HintIgnored => Self::HintIgnored,
        }
    }
}

/// Why a video receiver's target is what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AvVideoReason {
    /// No packets arrived; the track is left alone.
    NotFlowing,
    /// The track flows but reported no delay; nothing is changed.
    NoReading,
    /// There is no measured audio delay to follow.
    NoSound,
    /// This device cannot set a video receiver's delay.
    NoSetter,
    /// The engine synchronises this track itself; it is only observed.
    EngineSynced,
    /// The track began to flow: it starts at the audio's delay.
    Started,
    /// The skew is inside the dead band.
    Aligned,
    /// The target is being raised: the picture waits for the sound.
    Raising,
    /// The target is being lowered towards the sound.
    Lowering,
    /// The target stands and the receiver has not answered it yet.
    Settling,
    /// The engine does not follow the target; the picture stays early.
    HintIgnored,
    /// The track needs more delay than the sound has; the sound waits.
    SoundWaits,
    /// The track is later than the sound and stays so.
    PictureLate,
}

impl From<RustAvVideoReason> for AvVideoReason {
    fn from(r: RustAvVideoReason) -> Self {
        match r {
            RustAvVideoReason::NotFlowing => Self::NotFlowing,
            RustAvVideoReason::NoReading => Self::NoReading,
            RustAvVideoReason::NoSound => Self::NoSound,
            RustAvVideoReason::NoSetter => Self::NoSetter,
            RustAvVideoReason::EngineSynced => Self::EngineSynced,
            RustAvVideoReason::Started => Self::Started,
            RustAvVideoReason::Aligned => Self::Aligned,
            RustAvVideoReason::Raising => Self::Raising,
            RustAvVideoReason::Lowering => Self::Lowering,
            RustAvVideoReason::Settling => Self::Settling,
            RustAvVideoReason::HintIgnored => Self::HintIgnored,
            RustAvVideoReason::SoundWaits => Self::SoundWaits,
            RustAvVideoReason::PictureLate => Self::PictureLate,
        }
    }
}

impl From<AvVideoReason> for RustAvVideoReason {
    fn from(r: AvVideoReason) -> Self {
        match r {
            AvVideoReason::NotFlowing => Self::NotFlowing,
            AvVideoReason::NoReading => Self::NoReading,
            AvVideoReason::NoSound => Self::NoSound,
            AvVideoReason::NoSetter => Self::NoSetter,
            AvVideoReason::EngineSynced => Self::EngineSynced,
            AvVideoReason::Started => Self::Started,
            AvVideoReason::Aligned => Self::Aligned,
            AvVideoReason::Raising => Self::Raising,
            AvVideoReason::Lowering => Self::Lowering,
            AvVideoReason::Settling => Self::Settling,
            AvVideoReason::HintIgnored => Self::HintIgnored,
            AvVideoReason::SoundWaits => Self::SoundWaits,
            AvVideoReason::PictureLate => Self::PictureLate,
        }
    }
}

/// What to do with the audio receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AvAudioPlan {
    /// The target delay to set on the receiver now, milliseconds; `None`
    /// leaves it alone. `0` gives the target back.
    pub set_ms: Option<u32>,
    /// What the receiver was last told, milliseconds; `0` when nothing.
    pub asked_ms: u32,
    /// Why.
    pub reason: AvAudioReason,
}

impl From<RustAvAudioPlan> for AvAudioPlan {
    fn from(p: RustAvAudioPlan) -> Self {
        Self {
            set_ms: p.set_ms,
            asked_ms: p.asked_ms,
            reason: p.reason.into(),
        }
    }
}

/// What to do with one video receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AvVideoPlan {
    /// The target delay to set on the receiver now, milliseconds; `None`
    /// leaves it alone. `0` gives the target back.
    pub set_ms: Option<u32>,
    /// What the receiver was last told, milliseconds; `0` when nothing.
    pub asked_ms: u32,
    /// The track's measured delay minus the audio's, milliseconds: positive
    /// when the picture is later than the sound. `None` when either was not
    /// measured in this interval.
    pub skew_ms: Option<i32>,
    /// Why.
    pub reason: AvVideoReason,
}

impl From<RustAvVideoPlan> for AvVideoPlan {
    fn from(p: RustAvVideoPlan) -> Self {
        Self {
            set_ms: p.set_ms,
            asked_ms: p.asked_ms,
            skew_ms: p.skew_ms,
            reason: p.reason.into(),
        }
    }
}

/// One interval's answer for one remote person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AvPlan {
    /// The audio receiver.
    pub audio: AvAudioPlan,
    /// The main video track.
    pub main: AvVideoPlan,
    /// The middle video track.
    pub middle: AvVideoPlan,
    /// The light video track.
    pub light: AvVideoPlan,
}

impl From<RustAvPlan> for AvPlan {
    fn from(p: RustAvPlan) -> Self {
        Self {
            audio: p.audio.into(),
            main: p.main.into(),
            middle: p.middle.into(),
            light: p.light.into(),
        }
    }
}

/// What the rule can achieve on a device, from which receivers' delays the
/// device can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AvCapability {
    /// Both can be set: an early picture waits for the sound, and the sound
    /// waits (within its bound) for a picture that needs more.
    Full,
    /// Only video can be set: the audio cannot be delayed here, so a picture
    /// that needs more delay than the sound has stays late.
    PictureWaitsOnly,
    /// Only audio can be set: a picture that runs ahead of the sound cannot
    /// be held back here.
    SoundWaitsOnly,
    /// Neither can be set: the skew is measured and nothing is done.
    ObserveOnly,
}

impl From<RustAvCapability> for AvCapability {
    fn from(c: RustAvCapability) -> Self {
        match c {
            RustAvCapability::Full => Self::Full,
            RustAvCapability::PictureWaitsOnly => Self::PictureWaitsOnly,
            RustAvCapability::SoundWaitsOnly => Self::SoundWaitsOnly,
            RustAvCapability::ObserveOnly => Self::ObserveOnly,
        }
    }
}

impl From<AvCapability> for RustAvCapability {
    fn from(c: AvCapability) -> Self {
        match c {
            AvCapability::Full => Self::Full,
            AvCapability::PictureWaitsOnly => Self::PictureWaitsOnly,
            AvCapability::SoundWaitsOnly => Self::SoundWaitsOnly,
            AvCapability::ObserveOnly => Self::ObserveOnly,
        }
    }
}

/// Keeps one remote person's sound and picture together in a group call:
/// from what this device's receivers report each interval, the target
/// playout delay to set on each of them.
///
/// One per remote person. Every [`av_align_reading_ms`] the client reads its
/// receivers' statistics and calls [`AvAlign::read`] with one clock in whole
/// milliseconds (any origin), then sets every `set_ms` that came back.
/// Thread-safe.
#[derive(uniffi::Object)]
pub struct AvAlign {
    inner: Mutex<RustAvAlign>,
}

impl AvAlign {
    /// The state is plain numbers and no method can panic while holding the
    /// lock, so a poisoned lock still guards a consistent state.
    fn state(&self) -> MutexGuard<'_, RustAvAlign> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[uniffi::export]
impl AvAlign {
    /// A remote person's alignment on a device that can (or cannot) set the
    /// target delay of an audio receiver and of a video receiver.
    #[uniffi::constructor]
    #[must_use]
    pub fn new(audio_settable: bool, video_settable: bool) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RustAvAlign::new(audio_settable, video_settable)),
        })
    }

    /// One interval's readings, taken at `now_ms`. Returns what to set on
    /// each receiver.
    pub fn read(&self, now_ms: u64, readings: AvReadings) -> AvPlan {
        self.state().read(now_ms, &readings.into()).into()
    }

    /// The receivers were replaced: nothing has been told to the new ones.
    /// The next reading starts every track again and tells every receiver
    /// its target.
    pub fn receivers_replaced(&self) {
        self.state().receivers_replaced();
    }

    /// What the rule can achieve on this device.
    #[must_use]
    pub fn capability(&self) -> AvCapability {
        self.state().capability().into()
    }
}

/// What the rule can and cannot achieve with the setters a device has.
#[uniffi::export]
#[must_use]
pub fn av_align_capability(audio_settable: bool, video_settable: bool) -> AvCapability {
    rust_av_align_capability(audio_settable, video_settable).into()
}

/// The word a diagnostic line uses for a capability, the same on every
/// client.
#[uniffi::export]
#[must_use]
pub fn av_capability_name(capability: AvCapability) -> String {
    RustAvCapability::from(capability).name().to_string()
}

/// The word a diagnostic line uses for an audio reason, the same on every
/// client.
#[uniffi::export]
#[must_use]
pub fn av_audio_reason_name(reason: AvAudioReason) -> String {
    RustAvAudioReason::from(reason).name().to_string()
}

/// The word a diagnostic line uses for a video reason, the same on every
/// client.
#[uniffi::export]
#[must_use]
pub fn av_video_reason_name(reason: AvVideoReason) -> String {
    RustAvVideoReason::from(reason).name().to_string()
}

/// How often a client reads its receivers and calls [`AvAlign::read`],
/// milliseconds. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn av_align_reading_ms() -> u64 {
    AV_READING_MS
}

/// The skew inside which a track is left as it is, milliseconds. Chosen,
/// not measured.
#[uniffi::export]
#[must_use]
pub fn av_align_dead_band_ms() -> u32 {
    AV_DEAD_BAND_MS
}

/// The most the sound is asked to wait for a picture above what the audio
/// needs for itself, milliseconds. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn av_align_sound_wait_max_ms() -> u32 {
    AV_SOUND_WAIT_MAX_MS
}

/// What a [`SpeakingTracker`] says after a decision: whose ring is on and who
/// holds the floor (`docs/ux/group-call-grid.md` sections 5.2 and 7.3).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SpeakingSnapshot {
    /// Everyone whose ring is on, this device's own person included, sorted
    /// by id.
    pub speaking_ids: Vec<String>,
    /// The one person holding the floor, or none. Never this device's own
    /// person.
    pub active_speaker_id: Option<String>,
    /// When each person who has held the floor last held it, on the caller's
    /// clock.
    pub last_spoke_at_ms: HashMap<String, u64>,
}

fn speaking_snapshot(tracker: &RustSpeakingTracker) -> SpeakingSnapshot {
    SpeakingSnapshot {
        speaking_ids: tracker.speaking().into_iter().map(str::to_owned).collect(),
        active_speaker_id: tracker.floor().map(str::to_owned),
        last_spoke_at_ms: tracker
            .floor_history()
            .into_iter()
            .map(|(id, at)| (id.to_owned(), at))
            .collect(),
    }
}

/// Who is speaking in a group call: the ring on each tile and the one person
/// holding the floor, decided by one rule for the three clients.
///
/// One per call. For every person of the roster, at every reading, call
/// [`SpeakingTracker::sample`] with the level that was read (or none when it
/// could not be read), or [`SpeakingTracker::silence`] while they are muted;
/// then [`SpeakingTracker::decide_floor`] with the roster order. Levels are
/// the root mean square of decoded samples, `0..1`; times are one clock in
/// whole milliseconds, any origin. Thread-safe.
#[derive(uniffi::Object)]
pub struct SpeakingTracker {
    inner: Mutex<RustSpeakingTracker>,
}

impl SpeakingTracker {
    /// No method can panic while holding the lock, so a poisoned lock still
    /// guards a consistent state.
    fn state(&self) -> MutexGuard<'_, RustSpeakingTracker> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[uniffi::export]
impl SpeakingTracker {
    /// A tracker of a call nobody has been heard in.
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RustSpeakingTracker::new()),
        })
    }

    /// Name this device's own person (or none). They are sampled and have a
    /// ring like anyone, and never hold the floor, wherever they stand in
    /// the order given to [`SpeakingTracker::decide_floor`].
    pub fn set_local(&self, person: Option<String>) {
        self.state().set_local(person.as_deref());
    }

    /// One level reading for one person at `now_ms`. No level is "could not
    /// be read": silence for the ring, and nothing for the person's noise
    /// floor. A level that is not a number is silence too.
    pub fn sample(&self, person: String, level: Option<f64>, now_ms: u64) {
        self.state().sample(&person, level, now_ms);
    }

    /// The person is muted: their ring goes off at once and they give up the
    /// floor. What was learned about their room is kept.
    pub fn silence(&self, person: String) {
        self.state().silence(&person);
    }

    /// The person left the call: everything about them is dropped.
    pub fn forget(&self, person: String) {
        self.state().forget(&person);
    }

    /// Forget everyone who is not one of `people`, the roster as it is now.
    pub fn keep_only(&self, people: Vec<String>) {
        self.state().keep_only(&people);
    }

    /// Settle who holds the floor after the samples so far. `order` is the
    /// roster in order, which breaks a tie; this device's own person may be
    /// in it and is skipped.
    pub fn decide_floor(&self, order: Vec<String>, now_ms: u64) -> SpeakingSnapshot {
        let mut state = self.state();
        state.decide_floor(&order, now_ms);
        speaking_snapshot(&state)
    }

    /// The rings and the floor as last decided, without deciding again.
    #[must_use]
    pub fn snapshot(&self) -> SpeakingSnapshot {
        speaking_snapshot(&self.state())
    }

    /// Whether the person's ring is on.
    #[must_use]
    pub fn is_speaking(&self, person: String) -> bool {
        self.state().is_speaking(&person)
    }

    /// What counts as sound for the person now, on the scale of the levels
    /// (for the diagnostics line).
    #[must_use]
    pub fn threshold(&self, person: String) -> f64 {
        self.state().threshold(&person)
    }
}

/// The threshold a person starts with, before anything is known about the
/// noise around their microphone. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn speaking_level() -> f64 {
    SPEAKING_LEVEL
}

/// The lowest a person's threshold goes. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn speaking_level_min() -> f64 {
    SPEAKING_LEVEL_MIN
}

/// The highest a person's threshold goes. Chosen, not measured.
#[uniffi::export]
#[must_use]
pub fn speaking_level_max() -> f64 {
    SPEAKING_LEVEL_MAX
}

/// Sound for this long turns the ring on, milliseconds. Chosen, not
/// measured.
#[uniffi::export]
#[must_use]
pub fn speaking_ring_on_ms() -> u64 {
    RING_ON_MS
}

/// Silence for this long turns the ring off, milliseconds. Chosen, not
/// measured.
#[uniffi::export]
#[must_use]
pub fn speaking_ring_off_ms() -> u64 {
    RING_OFF_MS
}

/// Continuous speech for this long takes the floor, milliseconds. Chosen,
/// not measured.
#[uniffi::export]
#[must_use]
pub fn speaking_floor_onset_ms() -> u64 {
    FLOOR_ONSET_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_av_alignment_crosses_the_boundary_intact() {
        assert_eq!(
            (
                av_align_reading_ms(),
                av_align_dead_band_ms(),
                av_align_sound_wait_max_ms()
            ),
            (1000, 30, 300)
        );
        assert_eq!(
            av_align_capability(false, true),
            AvCapability::PictureWaitsOnly
        );
        assert_eq!(
            av_capability_name(AvCapability::PictureWaitsOnly),
            "picture-waits-only"
        );
        assert_eq!(
            av_audio_reason_name(AvAudioReason::WaitBounded),
            "wait-bounded"
        );
        assert_eq!(
            av_video_reason_name(AvVideoReason::SoundWaits),
            "sound-waits"
        );
        let absent = AvVideoReading {
            flowing: false,
            shown: false,
            engine_synced: false,
            delay_ms: None,
            network_ms: None,
        };
        let readings = |sound: u32, light: u32| AvReadings {
            audio: AvAudioReading {
                flowing: true,
                delay_ms: Some(sound),
                network_ms: None,
            },
            main: AvVideoReading {
                flowing: true,
                shown: false,
                engine_synced: true,
                delay_ms: Some(sound + 40),
                network_ms: None,
            },
            middle: absent,
            light: AvVideoReading {
                flowing: true,
                shown: true,
                engine_synced: false,
                delay_ms: Some(light),
                network_ms: None,
            },
        };
        let a = AvAlign::new(true, true);
        assert_eq!(a.capability(), AvCapability::Full);
        // Light at the 290 ms floor, the sound at 450: light starts at the
        // sound's delay; main is observed only.
        let plan = a.read(0, readings(450, 290));
        assert_eq!(plan.light.set_ms, Some(450));
        assert_eq!(plan.light.skew_ms, Some(-160));
        assert_eq!(plan.light.reason, AvVideoReason::Started);
        assert_eq!(plan.main.set_ms, None);
        assert_eq!(plan.main.skew_ms, Some(40));
        assert_eq!(plan.main.reason, AvVideoReason::EngineSynced);
        assert_eq!(plan.middle.reason, AvVideoReason::NotFlowing);
        assert_eq!(plan.audio.set_ms, None);
        assert_eq!(plan.audio.reason, AvAudioReason::Free);
        // The sound jumps to 990: 240 ms a second upwards.
        let plan = a.read(1000, readings(990, 450));
        assert_eq!(plan.light.set_ms, Some(690));
        assert_eq!(plan.light.asked_ms, 690);
        assert_eq!(plan.light.reason, AvVideoReason::Raising);
        // New receivers are told again.
        a.receivers_replaced();
        assert_eq!(a.read(2000, readings(990, 690)).light.set_ms, Some(990));
    }

    #[test]
    fn the_tile_video_rule_crosses_the_boundary_intact() {
        assert_eq!(
            (
                tile_main_min_width_px(),
                tile_light_max_width_px(),
                middle_sender_min_people()
            ),
            (960, 240, 3)
        );
        assert_eq!(tile_video_quality(2, 100, false), TileVideoQuality::Main);
        assert_eq!(tile_video_quality(3, 960, true), TileVideoQuality::Main);
        assert_eq!(tile_video_quality(3, 959, true), TileVideoQuality::Middle);
        assert_eq!(tile_video_quality(3, 2000, false), TileVideoQuality::Middle);
        assert_eq!(tile_video_quality(3, 240, false), TileVideoQuality::Middle);
        assert_eq!(tile_video_quality(3, 239, false), TileVideoQuality::Light);
        assert_eq!(tile_video_quality(3, 239, true), TileVideoQuality::Light);
        assert!(!middle_sender_wanted(2));
        assert!(middle_sender_wanted(3));
    }

    #[test]
    fn the_layer_display_crosses_the_boundary_intact() {
        assert_eq!(
            (
                layer_full_stall_ms(),
                layer_light_fresh_ms(),
                layer_full_steady_ms(),
                layer_first_frame_grace_ms(),
                layer_tick_ms()
            ),
            (150, 250, 1000, 1500, 50)
        );
        let d = LayerDisplay::new(0, true);
        for t in (0..=1000).step_by(20) {
            assert_eq!(d.full_frame(t), ShownLayer::Full);
            assert_eq!(d.light_frame(t), ShownLayer::Full);
        }
        for t in (1020..=1280).step_by(20) {
            d.light_frame(t);
        }
        // 300 ms without a full frame is where the tile hands over.
        assert_eq!(d.tick(1299), ShownLayer::Full);
        assert_eq!(d.tick(1330), ShownLayer::Light);
        assert_eq!(d.shown(), ShownLayer::Light);
        let total = d.counters();
        assert_eq!(
            (
                total.light_switches,
                total.stall_max_ms,
                total.stall_total_ms
            ),
            (1, 330, 330)
        );
        assert_eq!(d.drain(), total);
        assert_eq!(d.drain().stall_total_ms, 0);
        assert_eq!(d.set_light_available(false, 1350), ShownLayer::Full);
        assert_eq!(d.resync(5000), ShownLayer::Full);
        assert_eq!(add_layer_display_counters(total, total).light_switches, 2);
        assert_eq!(add_layer_display_counters(total, total).stall_max_ms, 330);
    }

    #[test]
    fn visibility_transitions_cross_the_boundary_intact() {
        use crate::scoped_discovery::{default_visibility_setting, FfiScopeKind};
        let s = default_visibility_setting();
        let nobody = choose_visibility_mode(s.clone(), FfiVisibilityMode::Nobody);
        assert_eq!(nobody.mode, FfiVisibilityMode::Nobody);
        assert!(nobody.last_contacts_of_contacts);
        let back = choose_visibility_mode(nobody, FfiVisibilityMode::Some);
        assert_eq!(back, s);

        let off = set_contacts_of_contacts(set_groups_visible(s.clone(), false), false);
        assert_eq!(
            off.mode,
            FfiVisibilityMode::Nobody,
            "the last part off is Nobody"
        );

        let scope = |b: u8| FfiScopeId {
            kind: FfiScopeKind::Group,
            id: vec![b; 8],
        };
        let excluded =
            exclude_scope_under_all(s.clone(), vec![scope(1), scope(2)], scope(2)).unwrap();
        assert_eq!(excluded.groups, Some(FfiGroupsForm::Chosen));
        assert_eq!(excluded.chosen, vec![scope(1)]);
        let chosen = set_scope_chosen(excluded, scope(3), true).unwrap();
        assert_eq!(chosen.chosen, vec![scope(1), scope(3)]);
        assert_eq!(
            set_groups_form(chosen, FfiGroupsForm::All).groups,
            Some(FfiGroupsForm::All)
        );
        assert!(matches!(
            set_scope_chosen(
                s,
                FfiScopeId {
                    kind: FfiScopeKind::Channel,
                    id: vec![]
                },
                true
            ),
            Err(FfiError::Visibility(_))
        ));
    }

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
        assert!(channel_info_sections(ChannelInfoCapabilities {
            is_publisher: true,
            is_subscriber: false,
            subscriber_count_known: true,
        })
        .contains(&InfoSection::ChannelMembers));
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

    #[test]
    fn the_speaking_rule_crosses_the_boundary_intact() {
        assert_eq!(
            (
                speaking_ring_on_ms(),
                speaking_ring_off_ms(),
                speaking_floor_onset_ms()
            ),
            (200, 800, 1500)
        );
        assert!(speaking_level().to_bits() == 0.02_f64.to_bits());
        assert!(speaking_level_min().to_bits() == 0.005_f64.to_bits());
        assert!(speaking_level_max().to_bits() == 0.05_f64.to_bits());
        let t = SpeakingTracker::new();
        t.set_local(Some("me".into()));
        let order = vec!["me".to_string(), "a".to_string(), "b".to_string()];
        let mut last = t.snapshot();
        assert_eq!(last.active_speaker_id, None);
        for now in (0..=1500).step_by(150) {
            t.sample("me".into(), Some(0.4), now);
            t.sample("b".into(), Some(0.3), now);
            t.sample("a".into(), None, now);
            last = t.decide_floor(order.clone(), now);
            // The first reading at or past 200 ms is the one at 300.
            assert_eq!(t.is_speaking("b".into()), now >= 300);
        }
        assert_eq!(last.speaking_ids, vec!["b".to_string(), "me".to_string()]);
        assert_eq!(last.active_speaker_id.as_deref(), Some("b"));
        assert_eq!(last.last_spoke_at_ms, HashMap::from([("b".into(), 1500)]));
        assert_eq!(t.snapshot(), last);
        assert!(t.threshold("a".into()).to_bits() == speaking_level().to_bits());
        // A quiet room pulls the threshold down; a mute keeps it.
        for now in (1650..=4500).step_by(150) {
            t.sample("a".into(), Some(0.002), now);
        }
        assert!((t.threshold("a".into()) - 0.006).abs() < 1e-9);
        t.silence("b".into());
        let muted = t.decide_floor(order.clone(), 4500);
        assert_eq!(muted.speaking_ids, vec!["me".to_string()]);
        assert_eq!(muted.active_speaker_id, None);
        assert_eq!(muted.last_spoke_at_ms.get("b"), Some(&1500));
        t.forget("b".into());
        assert!(t.snapshot().last_spoke_at_ms.is_empty());
        t.keep_only(vec!["a".into()]);
        assert!(t.snapshot().speaking_ids.is_empty());
        // Nothing a caller can hand over panics.
        t.sample(String::new(), Some(f64::NAN), u64::MAX);
        t.sample("a".into(), Some(f64::INFINITY), 0);
        t.decide_floor(Vec::new(), 0);
    }
}
