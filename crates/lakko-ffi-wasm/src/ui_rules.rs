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
    GroupInfoCapabilities as RustGroupInfoCapabilities, InfoTarget as RustInfoTarget,
    PersonInfoCapabilities as RustPersonInfoCapabilities,
};
use lakko_ui_rules::lookup::{
    bare_name as rust_bare_name, looks_like_a_name as rust_looks_like_a_name,
};
use lakko_ui_rules::ring::{
    remaining_ring_ms as rust_remaining_ring_ms, ring_decision as rust_ring_decision,
    RingDecision as RustRingDecision, CALL_RING_WINDOW_MS,
};
use lakko_ui_rules::visibility::{
    choose_mode as rust_choose_mode, exclude_scope_under_all as rust_exclude_scope_under_all,
    set_contacts_of_contacts as rust_set_contacts_of_contacts, set_groups as rust_set_groups,
    set_groups_form as rust_set_groups_form, set_scope_chosen as rust_set_scope_chosen,
};
use wasm_bindgen::prelude::*;

use crate::scoped_discovery::{GroupsForm, Refusal, ScopeId, VisibilityMode, VisibilitySetting};
use lakko_contacts::visibility::ScopeId as RustScopeId;

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
#[allow(clippy::struct_excessive_bools)]
pub struct ConversationRow {
    id: String,
    kind: ConversationKind,
    title: String,
    last_activity_ms: Option<i64>,
    pinned: bool,
    archived: bool,
    muted: bool,
    has_unread: bool,
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
    ///
    /// `muted` and `hasUnread` (ADR-0058) are read only by the folder rule;
    /// both are optional and default to `false`, so a six-argument call
    /// stays valid. `hasUnread` means what the unread badge means: something
    /// unread, or a manual "mark as unread".
    #[wasm_bindgen(constructor)]
    #[must_use]
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    pub fn new(
        id: String,
        kind: ConversationKind,
        title: String,
        last_activity_ms: Option<i64>,
        pinned: bool,
        archived: bool,
        muted: Option<bool>,
        has_unread: Option<bool>,
    ) -> Self {
        Self {
            id,
            kind,
            title,
            last_activity_ms,
            pinned,
            archived,
            muted: muted.unwrap_or(false),
            has_unread: has_unread.unwrap_or(false),
        }
    }

    /// Muted by this person.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// Something unread in it, or marked unread by hand.
    #[wasm_bindgen(getter, js_name = hasUnread)]
    #[must_use]
    pub fn has_unread(&self) -> bool {
        self.has_unread
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
            muted: r.muted,
            has_unread: r.has_unread,
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

/// A chat folder's rule in row terms (ADR-0058 section 6).
///
/// `includeKinds` / `excludeFlags` are the folder's bits as the synced
/// state returns them (include: bit0 one-to-one, bit1 groups, bit2
/// channels; exclude: bit0 muted, bit1 read, bit2 archived). `included` /
/// `excluded` are the row ids the client maps the folder's hand-added and
/// hand-excluded references to.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderRule {
    inner: RustFolderRule,
}

#[wasm_bindgen]
impl FolderRule {
    /// Build the rule.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(
        include_kinds: u32,
        exclude_flags: u32,
        included: Vec<String>,
        excluded: Vec<String>,
    ) -> Self {
        Self {
            inner: RustFolderRule {
                include_kinds,
                exclude_flags,
                included,
                excluded,
            },
        }
    }
}

/// Whether `row` is in the folder: excluded by hand beats everything; then
/// included by kind toggle or by hand; then not muted / read / archived
/// while the matching exclude toggle is on (`docs/ux/chat-folders.md` 3.2).
/// Borrows both arguments; neither is consumed.
#[wasm_bindgen(js_name = folderMatches)]
#[must_use]
pub fn folder_matches(folder: &FolderRule, row: &ConversationRow) -> bool {
    rust_folder_matches(&folder.inner, &row.clone().into())
}

/// Order a folder's rows: the folder's own pins first in their stored
/// order (`folderPins` are row ids; unknown ids skipped), then the
/// `orderConversations` rule with the global pin ignored. Returns input
/// indices, every row exactly once. Consumes the rows, like
/// `orderConversations`.
#[wasm_bindgen(js_name = orderConversationsInFolder)]
#[must_use]
pub fn order_conversations_in_folder(
    rows: Vec<ConversationRow>,
    folder_pins: Vec<String>,
) -> Vec<u32> {
    let rust: Vec<RustConversationRow> = rows.into_iter().map(Into::into).collect();
    rust_order_conversations_in_folder(&rust, &folder_pins)
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

/// Which info a conversation's name opens — `docs/ux/conversation-info.md`
/// section 1.
#[wasm_bindgen(js_name = InfoTarget)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoTarget {
    /// A conversation between two people opens the other person.
    Person = 0,
    /// A group opens the group.
    Group = 1,
    /// A channel opens the channel.
    Channel = 2,
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

/// What the viewer is to a one-to-one conversation.
///
/// A constructible class rather than two positional booleans on the call, so
/// the reading of each flag is written down at the call site rather than
/// inferred from its position.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersonInfoCapabilities {
    membership_ended: bool,
}

#[wasm_bindgen]
impl PersonInfoCapabilities {
    /// `membershipEnded`: this device's membership in the conversation is
    /// over (ADR-0045 UX 6.2). A person's card is the contact card; there is
    /// no `isContact`.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(membership_ended: bool) -> Self {
        Self { membership_ended }
    }

    /// This device's membership in the conversation is over.
    #[wasm_bindgen(getter, js_name = membershipEnded)]
    #[must_use]
    pub fn membership_ended(&self) -> bool {
        self.membership_ended
    }
}

/// What the viewer may do in a group. Every field is a decision the client
/// already made from the live role table; none is re-derived here.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct GroupInfoCapabilities {
    can_manage_members: bool,
    can_rename: bool,
    can_mint_invite: bool,
    is_owner: bool,
    can_leave: bool,
    membership_ended: bool,
}

#[wasm_bindgen]
impl GroupInfoCapabilities {
    /// In order: owner-or-admin and still a member (ADR-0022), the same for
    /// renaming (ADR-0051) and for minting an invite (ADR-0028), the owner
    /// (ADR-0045), whether this device can actually leave (ADR-0045's member
    /// exit is a `Leave` proposal, so a group with no role table has no
    /// proposal path and nothing for the control to do), and whether this
    /// device's membership is over.
    #[wasm_bindgen(constructor)]
    #[must_use]
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn new(
        can_manage_members: bool,
        can_rename: bool,
        can_mint_invite: bool,
        is_owner: bool,
        can_leave: bool,
        membership_ended: bool,
    ) -> Self {
        Self {
            can_manage_members,
            can_rename,
            can_mint_invite,
            is_owner,
            can_leave,
            membership_ended,
        }
    }

    /// Owner or admin, and still a member.
    #[wasm_bindgen(getter, js_name = canManageMembers)]
    #[must_use]
    pub fn can_manage_members(&self) -> bool {
        self.can_manage_members
    }

    /// May change the shared name.
    #[wasm_bindgen(getter, js_name = canRename)]
    #[must_use]
    pub fn can_rename(&self) -> bool {
        self.can_rename
    }

    /// May mint an invite link.
    #[wasm_bindgen(getter, js_name = canMintInvite)]
    #[must_use]
    pub fn can_mint_invite(&self) -> bool {
        self.can_mint_invite
    }

    /// The owner.
    #[wasm_bindgen(getter, js_name = isOwner)]
    #[must_use]
    pub fn is_owner(&self) -> bool {
        self.is_owner
    }

    /// Whether this device can actually leave.
    #[wasm_bindgen(getter, js_name = canLeave)]
    #[must_use]
    pub fn can_leave(&self) -> bool {
        self.can_leave
    }

    /// This device's membership is over: read-only.
    #[wasm_bindgen(getter, js_name = membershipEnded)]
    #[must_use]
    pub fn membership_ended(&self) -> bool {
        self.membership_ended
    }
}

/// What the viewer is to a channel.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelInfoCapabilities {
    is_publisher: bool,
    is_subscriber: bool,
    subscriber_count_known: bool,
}

#[wasm_bindgen]
impl ChannelInfoCapabilities {
    /// In order: this device holds the publisher keys, this device reads the
    /// channel as a subscriber, and this device has a subscriber count —
    /// which only a publisher ever does.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(is_publisher: bool, is_subscriber: bool, subscriber_count_known: bool) -> Self {
        Self {
            is_publisher,
            is_subscriber,
            subscriber_count_known,
        }
    }

    /// This device holds the publisher keys.
    #[wasm_bindgen(getter, js_name = isPublisher)]
    #[must_use]
    pub fn is_publisher(&self) -> bool {
        self.is_publisher
    }

    /// This device reads the channel as a subscriber.
    #[wasm_bindgen(getter, js_name = isSubscriber)]
    #[must_use]
    pub fn is_subscriber(&self) -> bool {
        self.is_subscriber
    }

    /// This device has a subscriber count.
    #[wasm_bindgen(getter, js_name = subscriberCountKnown)]
    #[must_use]
    pub fn subscriber_count_known(&self) -> bool {
        self.subscriber_count_known
    }
}

/// Which info the header opens for a conversation of this kind.
#[wasm_bindgen(js_name = infoTarget)]
#[must_use]
pub fn info_target(kind: ConversationKind) -> InfoTarget {
    rust_info_target(kind.into()).into()
}

/// What a person's info holds — section 2.1.
///
/// The blocks come back as their kebab-case names, which is the vocabulary
/// the web client's own section type has always been written in — so the
/// names are handed over rather than translated, and a client re-spelling
/// one is no longer possible.
///
/// Nothing in the result is ever group-shaped, whatever the capabilities.
#[wasm_bindgen(js_name = personInfoSections)]
#[must_use]
pub fn person_info_sections(caps: PersonInfoCapabilities) -> Vec<String> {
    rust_person_info_sections(RustPersonInfoCapabilities {
        membership_ended: caps.membership_ended,
    })
    .into_iter()
    .map(|s| s.name().to_string())
    .collect()
}

/// What a group's info holds — section 2.2.
#[wasm_bindgen(js_name = groupInfoSections)]
#[must_use]
pub fn group_info_sections(caps: GroupInfoCapabilities) -> Vec<String> {
    rust_group_info_sections(RustGroupInfoCapabilities {
        can_manage_members: caps.can_manage_members,
        can_rename: caps.can_rename,
        can_mint_invite: caps.can_mint_invite,
        is_owner: caps.is_owner,
        can_leave: caps.can_leave,
        membership_ended: caps.membership_ended,
    })
    .into_iter()
    .map(|s| s.name().to_string())
    .collect()
}

/// What a channel's info holds — section 2.3.
#[wasm_bindgen(js_name = channelInfoSections)]
#[must_use]
pub fn channel_info_sections(caps: ChannelInfoCapabilities) -> Vec<String> {
    rust_channel_info_sections(RustChannelInfoCapabilities {
        is_publisher: caps.is_publisher,
        is_subscriber: caps.is_subscriber,
        subscriber_count_known: caps.subscriber_count_known,
    })
    .into_iter()
    .map(|s| s.name().to_string())
    .collect()
}

/// Where a call is, as the collapsed bar of `docs/ux/calls.md` section 4.5
/// needs to see it.
#[wasm_bindgen(js_name = CallBarPhase)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallBarPhase {
    /// No call at all.
    Idle = 0,
    /// An incoming call being offered, which nobody has answered.
    Ringing = 1,
    /// Built, not yet on the wire.
    Preparing = 2,
    /// Ringing out.
    Calling = 3,
    /// Answered; the transport is coming up.
    Connecting = 4,
    /// Media is flowing.
    Connected = 5,
    /// The transport dropped and is being rebuilt.
    Reconnecting = 6,
    /// Over.
    Ended = 7,
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

/// Whether the collapsed call bar is on screen.
///
/// `callScreenUp` is the client's own read of its call presentation. One
/// state, one place: the bar exists because the call screen is away.
#[wasm_bindgen(js_name = callBarVisible)]
#[must_use]
pub fn call_bar_visible(phase: CallBarPhase, call_screen_up: bool) -> bool {
    matches!(
        rust_call_bar_visibility(phase.into(), call_screen_up),
        RustCallBar::Shown
    )
}

/// What the collapsed bar's one status line says: `calling`, `connecting`,
/// `reconnecting`, or `elapsed` - and never two at once, which is why this
/// answers a single name rather than three booleans.
///
/// On `elapsed` the client formats {@link callBarElapsedMs}; how long a call
/// has been running is not a presentation decision, so the rule carries the
/// number through untouched.
#[wasm_bindgen(js_name = callBarStatusKind)]
#[must_use]
pub fn call_bar_status_kind(phase: CallBarPhase) -> String {
    match rust_call_bar_status(phase.into(), 0) {
        RustCallBarStatus::Calling => "calling",
        RustCallBarStatus::Connecting => "connecting",
        RustCallBarStatus::Reconnecting => "reconnecting",
        RustCallBarStatus::Elapsed(_) => "elapsed",
    }
    .to_owned()
}

/// The running time the bar shows, or `undefined` when its line carries a
/// word instead. Reading this without {@link callBarStatusKind} is the
/// mistake the pair exists to prevent: a line never says two things.
#[wasm_bindgen(js_name = callBarElapsedMs)]
#[must_use]
pub fn call_bar_elapsed_ms(phase: CallBarPhase, connected_for_ms: u64) -> Option<u64> {
    match rust_call_bar_status(phase.into(), connected_for_ms) {
        RustCallBarStatus::Elapsed(ms) => Some(ms),
        RustCallBarStatus::Calling
        | RustCallBarStatus::Connecting
        | RustCallBarStatus::Reconnecting => None,
    }
}

/// What an arriving call offer does to this device —
/// `docs/ux/calls.md` section 2.1.
#[wasm_bindgen(js_name = RingDecision)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingDecision {
    /// Ring: ringtone, vibration, and the call screen that takes over.
    Ring = 0,
    /// Record the call, interrupt nobody. The conversation is muted.
    Silent = 1,
    /// The caller has already given up. Record a missed call, show nothing.
    TooLate = 2,
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
#[wasm_bindgen(js_name = ringDecision)]
#[must_use]
pub fn ring_decision(muted: bool, offer_age_ms: u64) -> RingDecision {
    rust_ring_decision(muted, offer_age_ms).into()
}

/// How much of the ring window is left, in milliseconds, for an affordance
/// that needs a deadline rather than a verdict. Saturates at zero.
#[wasm_bindgen(js_name = remainingRingMs)]
#[must_use]
pub fn remaining_ring_ms(offer_age_ms: u64) -> u64 {
    rust_remaining_ring_ms(offer_age_ms)
}

/// How long a call rings before both sides give up, in milliseconds.
///
/// One copy, because it is a number two DIFFERENT devices must agree on.
#[wasm_bindgen(js_name = callRingWindowMs)]
#[must_use]
pub fn call_ring_window_ms() -> u64 {
    CALL_RING_WINDOW_MS
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
#[wasm_bindgen(js_name = callHoldsCapture)]
#[must_use]
pub fn call_holds_capture(call_is_live: bool, unanswered_incoming_ring: bool) -> bool {
    rust_call_holds_capture(call_is_live, unanswered_incoming_ring)
}

// ============================================================================
// "Who can find me" — the Settings transitions (UX section 11, ADR-0060 §7)
// ============================================================================
//
// Each takes the current setting and returns the next; none stamps
// `updatedAtMs` (call `withUpdatedAtMs`, then compose the envelope). Every
// result satisfies the setting's rule.

/// Choose one of the three positions. Leaving Some people remembers its
/// parts; coming back restores them (or the default parts).
#[wasm_bindgen(js_name = chooseVisibilityMode)]
#[must_use]
pub fn choose_visibility_mode(
    setting: &VisibilitySetting,
    mode: VisibilityMode,
) -> VisibilitySetting {
    VisibilitySetting::from_rust(rust_choose_mode(setting.rust(), mode.into()))
}

/// Switch part 2.1 (contacts of my contacts). Turning the last part off
/// moves to Nobody; turning a part on from Nobody or Everyone moves to Some
/// people.
#[wasm_bindgen(js_name = setContactsOfContacts)]
#[must_use]
pub fn set_contacts_of_contacts(setting: &VisibilitySetting, on: bool) -> VisibilitySetting {
    VisibilitySetting::from_rust(rust_set_contacts_of_contacts(setting.rust(), on))
}

/// Switch part 2.2 (groups and channels), keeping its form.
#[wasm_bindgen(js_name = setGroupsVisible)]
#[must_use]
pub fn set_groups_visible(setting: &VisibilitySetting, on: bool) -> VisibilitySetting {
    VisibilitySetting::from_rust(rust_set_groups(setting.rust(), on))
}

/// All groups and channels, or only the chosen ones. No effect unless part
/// 2.2 is on.
#[wasm_bindgen(js_name = setGroupsForm)]
#[must_use]
pub fn set_groups_form(setting: &VisibilitySetting, form: GroupsForm) -> VisibilitySetting {
    VisibilitySetting::from_rust(rust_set_groups_form(setting.rust(), form.into()))
}

fn exclude_scope_under_all_inner(
    setting: &VisibilitySetting,
    all_scopes: &[ScopeId],
    excluded: &ScopeId,
) -> Result<VisibilitySetting, Refusal> {
    let all: Vec<RustScopeId> = all_scopes.iter().map(Into::into).collect();
    Ok(VisibilitySetting::from_rust(rust_exclude_scope_under_all(
        setting.rust(),
        &all,
        &excluded.into(),
    )?))
}

/// Exclude one group or channel while "all" is on: the form becomes "only
/// the ones I choose" with every other scope in `allScopes` chosen.
///
/// # Errors
///
/// Throws an `Error` with `visibilityError` `scopeIdLength` /
/// `tooManyChosen`.
#[wasm_bindgen(js_name = excludeScopeUnderAll)]
pub fn exclude_scope_under_all(
    setting: &VisibilitySetting,
    all_scopes: Vec<ScopeId>,
    excluded: &ScopeId,
) -> Result<VisibilitySetting, JsValue> {
    exclude_scope_under_all_inner(setting, &all_scopes, excluded).map_err(Refusal::into_js)
}

fn set_scope_chosen_inner(
    setting: &VisibilitySetting,
    scope: &ScopeId,
    on: bool,
) -> Result<VisibilitySetting, Refusal> {
    Ok(VisibilitySetting::from_rust(rust_set_scope_chosen(
        setting.rust(),
        &scope.into(),
        on,
    )?))
}

/// Choose or unchoose one group or channel in the chosen list.
///
/// # Errors
///
/// Throws an `Error` with `visibilityError` `scopeIdLength` /
/// `tooManyChosen`.
#[wasm_bindgen(js_name = setScopeChosen)]
pub fn set_scope_chosen(
    setting: &VisibilitySetting,
    scope: &ScopeId,
    on: bool,
) -> Result<VisibilitySetting, JsValue> {
    set_scope_chosen_inner(setting, scope, on).map_err(Refusal::into_js)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_transitions_cross_the_boundary_intact() {
        use crate::scoped_discovery::{default_visibility_setting, ScopeKind};
        let s = default_visibility_setting();
        let nobody = choose_visibility_mode(&s, VisibilityMode::Nobody);
        assert_eq!(nobody.mode(), VisibilityMode::Nobody);
        assert_eq!(choose_visibility_mode(&nobody, VisibilityMode::Some), s);
        let off = set_contacts_of_contacts(&set_groups_visible(&s, false), false);
        assert_eq!(off.mode(), VisibilityMode::Nobody);

        let scope = |b: u8| ScopeId::new(ScopeKind::Group, vec![b; 8]);
        let excluded = exclude_scope_under_all(&s, vec![scope(1), scope(2)], &scope(2)).unwrap();
        assert_eq!(excluded.groups(), Some(GroupsForm::Chosen));
        assert_eq!(excluded.chosen(), vec![scope(1)]);
        let chosen = set_scope_chosen(&excluded, &scope(3), true).unwrap();
        assert_eq!(chosen.chosen(), vec![scope(1), scope(3)]);
        assert_eq!(
            set_groups_form(&chosen, GroupsForm::All).groups(),
            Some(GroupsForm::All)
        );
        let e = set_scope_chosen_inner(&s, &ScopeId::new(ScopeKind::Channel, vec![]), true)
            .unwrap_err();
        assert_eq!(
            (e.property, e.code.as_str()),
            ("visibilityError", "scopeIdLength")
        );
        assert!(exclude_scope_under_all_inner(
            &s,
            &[ScopeId::new(ScopeKind::Group, vec![])],
            &scope(1)
        )
        .is_err());
    }

    fn row(id: &str, kind: ConversationKind, at: Option<i64>) -> ConversationRow {
        ConversationRow::new(
            id.to_string(),
            kind,
            "Same".to_string(),
            at,
            false,
            false,
            None,
            None,
        )
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
    fn the_sections_cross_the_boundary_as_the_names_web_already_uses() {
        let ended = person_info_sections(PersonInfoCapabilities::new(true));
        assert!(ended.contains(&"delete-chat".to_string()));
        assert!(!ended.contains(&"call".to_string()));
        assert!(
            channel_info_sections(ChannelInfoCapabilities::new(false, true, false))
                .contains(&"unsubscribe".to_string())
        );
        assert!(
            channel_info_sections(ChannelInfoCapabilities::new(false, true, false))
                .contains(&"channel-members".to_string())
        );
        assert_eq!(info_target(ConversationKind::Group), InfoTarget::Group);
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
        assert!(looks_like_a_name("@anna"));
        assert!(!looks_like_a_name("ab"));
        assert_eq!(bare_name("  @anna "), "anna");
    }
}
