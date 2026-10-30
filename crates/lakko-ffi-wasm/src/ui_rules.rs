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

use lakko_ui_rules::av_align::{
    av_align_capability as rust_av_align_capability, AvAlign as RustAvAlign,
    AvAudioReading as RustAvAudioReading, AvAudioReason as RustAvAudioReason,
    AvCapability as RustAvCapability, AvPlan as RustAvPlan, AvReadings as RustAvReadings,
    AvVideoPlan as RustAvVideoPlan, AvVideoReading as RustAvVideoReading,
    AvVideoReason as RustAvVideoReason, AV_DEAD_BAND_MS, AV_READING_MS, AV_SOUND_WAIT_MAX_MS,
};
use lakko_ui_rules::call_bar::{
    call_bar_status as rust_call_bar_status, call_bar_visibility as rust_call_bar_visibility,
    CallBar as RustCallBar, CallBarPhase as RustCallBarPhase, CallBarStatus as RustCallBarStatus,
};
use lakko_ui_rules::call_group::{
    call_grid as rust_call_grid, call_stage as rust_call_stage,
    middle_sender_wanted as rust_middle_sender_wanted, ringback_plays as rust_ringback_plays,
    tile_video_quality as rust_tile_video_quality, CallGrid as RustCallGrid,
    StageCandidate as RustStageCandidate, TileVideoQuality as RustTileVideoQuality,
    LIGHT_MAX_WIDTH_PX, MAIN_MIN_WIDTH_PX, MIDDLE_SENDER_MIN_PEOPLE,
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
    choose_mode as rust_choose_mode, exclude_contact_under_all as rust_exclude_contact_under_all,
    exclude_scope_under_all as rust_exclude_scope_under_all,
    set_contact_chosen as rust_set_contact_chosen, set_contacts_form as rust_set_contacts_form,
    set_contacts_of_contacts as rust_set_contacts_of_contacts, set_groups as rust_set_groups,
    set_groups_form as rust_set_groups_form, set_scope_chosen as rust_set_scope_chosen,
};
use wasm_bindgen::prelude::*;

use crate::scoped_discovery::{
    flat_contact_ids, ContactsForm, GroupsForm, Refusal, ScopeId, VisibilityMode, VisibilitySetting,
};
use lakko_contacts::visibility::{contact_account_id, ScopeId as RustScopeId};

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

/// The name prefilled for a guest joining a call from a link:
/// "<adjective> <animal>". `random` is any random 32-bit value from the
/// caller (`crypto.getRandomValues`), enough to reach every pair.
#[wasm_bindgen(js_name = guestName)]
#[must_use]
pub fn guest_name(random: u32) -> String {
    lakko_ui_rules::guest_name::guest_name(u64::from(random))
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
    is_contact: bool,
    contacts_of_contacts_on: bool,
}

#[wasm_bindgen]
impl PersonInfoCapabilities {
    /// `membershipEnded`: this device's membership in the conversation is
    /// over (ADR-0045 UX 6.2). `isContact`: the person is on this device's
    /// contact list - `false` only for a person the viewer removed, whose
    /// one-to-one remains, and whose card then offers `add-contact`.
    /// `contactsOfContactsOn`: Contacts of my contacts is on in "Who can find
    /// me", under either form; left out it reads `false`, and the card has
    /// no `their-contacts-can-find-me` row.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(
        membership_ended: bool,
        is_contact: bool,
        contacts_of_contacts_on: Option<bool>,
    ) -> Self {
        Self {
            membership_ended,
            is_contact,
            contacts_of_contacts_on: contacts_of_contacts_on.unwrap_or(false),
        }
    }

    /// The person is on this device's contact list.
    #[wasm_bindgen(getter, js_name = isContact)]
    #[must_use]
    pub fn is_contact(&self) -> bool {
        self.is_contact
    }

    /// Contacts of my contacts is on in "Who can find me".
    #[wasm_bindgen(getter, js_name = contactsOfContactsOn)]
    #[must_use]
    pub fn contacts_of_contacts_on(&self) -> bool {
        self.contacts_of_contacts_on
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
        is_contact: caps.is_contact,
        contacts_of_contacts_on: caps.contacts_of_contacts_on,
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

/// All my contacts, or only the chosen ones. No effect unless part 2.1 is
/// on.
#[wasm_bindgen(js_name = setContactsForm)]
#[must_use]
pub fn set_contacts_form(setting: &VisibilitySetting, form: ContactsForm) -> VisibilitySetting {
    VisibilitySetting::from_rust(rust_set_contacts_form(setting.rust(), form.into()))
}

fn exclude_contact_under_all_inner(
    setting: &VisibilitySetting,
    all_contact_ids: &[u8],
    excluded: &[u8],
) -> Result<VisibilitySetting, Refusal> {
    let all = flat_contact_ids(all_contact_ids)?;
    let excluded = contact_account_id(excluded)?;
    Ok(VisibilitySetting::from_rust(
        rust_exclude_contact_under_all(setting.rust(), &all, &excluded)?,
    ))
}

/// Exclude one contact while "all" is on: the form becomes "only the ones I
/// choose" with every other contact in `allContactIds` (concatenated 16-byte
/// account ids) chosen.
///
/// # Errors
///
/// Throws an `Error` with `visibilityError` `contactIdLength` /
/// `tooManyChosenContacts`.
#[wasm_bindgen(js_name = excludeContactUnderAll)]
pub fn exclude_contact_under_all(
    setting: &VisibilitySetting,
    all_contact_ids: &[u8],
    excluded: &[u8],
) -> Result<VisibilitySetting, JsValue> {
    exclude_contact_under_all_inner(setting, all_contact_ids, excluded).map_err(Refusal::into_js)
}

fn set_contact_chosen_inner(
    setting: &VisibilitySetting,
    account_id: &[u8],
    on: bool,
) -> Result<VisibilitySetting, Refusal> {
    let id = contact_account_id(account_id)?;
    Ok(VisibilitySetting::from_rust(rust_set_contact_chosen(
        setting.rust(),
        &id,
        on,
    )?))
}

/// Choose or unchoose one contact in the chosen list.
///
/// # Errors
///
/// Throws an `Error` with `visibilityError` `contactIdLength` /
/// `tooManyChosenContacts`.
#[wasm_bindgen(js_name = setContactChosen)]
pub fn set_contact_chosen(
    setting: &VisibilitySetting,
    account_id: &[u8],
    on: bool,
) -> Result<VisibilitySetting, JsValue> {
    set_contact_chosen_inner(setting, account_id, on).map_err(Refusal::into_js)
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

/// Whether the outgoing tone loops (`docs/ux/calls.md` section 3.4).
///
/// Only while a one-to-one call this device placed waits for an answer. A
/// group call, or a call joined from a link, rings nobody.
#[wasm_bindgen(js_name = ringbackPlays)]
#[must_use]
pub fn ringback_plays(group_call: bool, phase: CallBarPhase) -> bool {
    rust_ringback_plays(group_call, phase.into())
}

/// How a group call's grid is drawn (`docs/ux/group-call-grid.md` section 4):
/// equal tiles filling the call area, with no scrolling.
#[wasm_bindgen(js_name = CallGrid)]
#[derive(Clone, Copy, Debug)]
pub struct CallGrid {
    inner: RustCallGrid,
}

#[wasm_bindgen(js_class = CallGrid)]
impl CallGrid {
    /// Columns on a page.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn cols(&self) -> u32 {
        self.inner.cols
    }

    /// Rows on a full page.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn rows(&self) -> u32 {
        self.inner.rows
    }

    /// Width of every tile.
    #[wasm_bindgen(getter, js_name = tileWidth)]
    #[must_use]
    pub fn tile_width(&self) -> u32 {
        self.inner.tile_width
    }

    /// Height of every tile.
    #[wasm_bindgen(getter, js_name = tileHeight)]
    #[must_use]
    pub fn tile_height(&self) -> u32 {
        self.inner.tile_height
    }

    /// The space between tiles.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn gap(&self) -> u32 {
        self.inner.gap
    }

    /// Tiles on a full page.
    #[wasm_bindgen(getter, js_name = perPage)]
    #[must_use]
    pub fn per_page(&self) -> u32 {
        self.inner.per_page
    }

    /// Pages; 1 when everyone fits.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn pages(&self) -> u32 {
        self.inner.pages
    }
}

/// Lay `count` equal tiles into a call area `width` by `height` (the space
/// between the status line and the control bar, inside the side margins).
#[wasm_bindgen(js_name = callGrid)]
#[must_use]
pub fn call_grid(count: u32, width: u32, height: u32) -> CallGrid {
    CallGrid {
        inner: rust_call_grid(count, width, height),
    }
}

/// Flag bit of {@link callStage}: this device's own tile.
pub const STAGE_FLAG_LOCAL: u8 = 1;
/// Flag bit of {@link callStage}: the person is sending a picture.
pub const STAGE_FLAG_CAMERA: u8 = 2;
/// Flag bit of {@link callStage}: the person is the active speaker now.
pub const STAGE_FLAG_SPEAKING: u8 = 4;
/// Flag bit of {@link callStage}: the tile is a shared screen (ADR-0083).
pub const STAGE_FLAG_SCREEN: u8 = 8;

/// Who is on the stage in the layout with one large tile: an index into the
/// roster, or `undefined` when the call is empty.
///
/// `flags` carries one byte per tile (bit 1: this device's own tile, bit 2:
/// camera on, bit 4: speaking now, bit 8: a shared screen) and `lastSpokeAtMs` one number per person
/// (0 for never), both in roster order. A `lastSpokeAtMs` shorter than
/// `flags` reads as "never" for the people it does not cover.
#[wasm_bindgen(js_name = callStage)]
#[must_use]
pub fn call_stage(flags: &[u8], last_spoke_at_ms: &[f64], pinned: Option<u32>) -> Option<u32> {
    let people: Vec<RustStageCandidate> = flags
        .iter()
        .enumerate()
        .map(|(index, bits)| RustStageCandidate {
            is_local: bits & STAGE_FLAG_LOCAL != 0,
            camera_on: bits & STAGE_FLAG_CAMERA != 0,
            speaking: bits & STAGE_FLAG_SPEAKING != 0,
            is_screen: bits & STAGE_FLAG_SCREEN != 0,
            last_spoke_at_ms: last_spoke_at_ms
                .get(index)
                .copied()
                .filter(|ms| ms.is_finite() && *ms > 0.0)
                // A millisecond clock fits a u64 with room to spare; the cast
                // saturates on anything larger.
                .map_or(0, |ms| {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let whole = ms as u64;
                    whole
                }),
        })
        .collect();
    let pinned = pinned.and_then(|p| usize::try_from(p).ok());
    rust_call_stage(&people, pinned).and_then(|index| u32::try_from(index).ok())
}

/// Which of a person's video tracks a call tile asks the call server for
/// (`docs/ux/group-call-grid.md` section 7.6, ADR-0070 step 5). The light
/// track always arrives beside it.
#[wasm_bindgen(js_name = TileVideoQuality)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileVideoQuality {
    /// The main track: the full picture.
    Main = 0,
    /// The middle track: the camera at half size.
    Middle = 1,
    /// The light track alone.
    Light = 2,
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
/// drawn at in **physical** pixels (CSS pixels times `devicePixelRatio`);
/// `pinned_or_stage` says the tile is the pinned person or the stage.
///
/// A call of two: `Main`. A pinned or stage tile at least
/// {@link tileMainMinWidthPx} wide: `Main`. Otherwise a tile narrower than
/// {@link tileLightMaxWidthPx}: `Light`. Otherwise: `Middle`.
#[wasm_bindgen(js_name = tileVideoQuality)]
#[must_use]
pub fn tile_video_quality(
    people: u32,
    tile_width_px: u32,
    pinned_or_stage: bool,
) -> TileVideoQuality {
    rust_tile_video_quality(people, tile_width_px, pinned_or_stage).into()
}

/// The name a `TileVideoQuality` has in the SFU's `video-wants` frame:
/// `main`, `middle` or `light` (what `composeSfuVideoWants` takes).
#[wasm_bindgen(js_name = tileVideoQualityName)]
#[must_use]
pub fn tile_video_quality_name(quality: TileVideoQuality) -> String {
    match quality {
        TileVideoQuality::Main => "main",
        TileVideoQuality::Middle => "middle",
        TileVideoQuality::Light => "light",
    }
    .to_owned()
}

/// Whether this device sends the middle copy of its camera: only from
/// {@link middleSenderMinPeople} people in the call on (its own person
/// included).
#[wasm_bindgen(js_name = middleSenderWanted)]
#[must_use]
pub fn middle_sender_wanted(people: u32) -> bool {
    rust_middle_sender_wanted(people)
}

/// A pinned or stage tile at least this wide, in physical pixels, asks for
/// the main track. Chosen, not measured.
#[wasm_bindgen(js_name = tileMainMinWidthPx)]
#[must_use]
pub fn tile_main_min_width_px() -> u32 {
    MAIN_MIN_WIDTH_PX
}

/// A tile narrower than this, in physical pixels, asks for the light track
/// alone. Chosen, not measured.
#[wasm_bindgen(js_name = tileLightMaxWidthPx)]
#[must_use]
pub fn tile_light_max_width_px() -> u32 {
    LIGHT_MAX_WIDTH_PX
}

/// A person sends the middle copy of their camera from this many people in
/// the call on. Chosen.
#[wasm_bindgen(js_name = middleSenderMinPeople)]
#[must_use]
pub fn middle_sender_min_people() -> u32 {
    MIDDLE_SENDER_MIN_PEOPLE
}

/// Which of a remote person's two video tracks a call tile shows
/// (`docs/ux/group-call-grid.md` section 7.5).
#[wasm_bindgen(js_name = ShownLayer)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShownLayer {
    /// The person's main picture.
    Full = 0,
    /// The light companion.
    Light = 1,
}

impl From<RustShownLayer> for ShownLayer {
    fn from(layer: RustShownLayer) -> Self {
        match layer {
            RustShownLayer::Full => Self::Full,
            RustShownLayer::Light => Self::Light,
        }
    }
}

/// A browser timestamp (`performance.now()`, a frame callback's `now`) as
/// whole milliseconds. Fractions are dropped; a negative or non-finite value
/// reads as 0 and the cast saturates on anything too large.
fn whole_ms(ms: f64) -> u64 {
    if !ms.is_finite() || ms <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = ms as u64;
    whole
}

/// Counters as the four numbers JavaScript reads, in the order
/// `[lightSwitches, lightMs, stallMaxMs, stallTotalMs]`. One typed array
/// rather than an object with getters: the tile drains on every tick, and
/// this is one copy with nothing to free.
fn counters_array(c: RustLayerDisplayCounters) -> Vec<f64> {
    // A count of milliseconds in a call is far below 2^53.
    #[allow(clippy::cast_precision_loss)]
    let out = vec![
        c.light_switches as f64,
        c.light_ms as f64,
        c.stall_max_ms as f64,
        c.stall_total_ms as f64,
    ];
    out
}

/// One remote tile's choice between the person's main video track and its
/// light companion: the tile never stands still while a lighter copy of the
/// picture is arriving.
///
/// One per remote tile. The tile reports every presented frame of each track
/// and calls `tick` every {@link layerTickMs}, all on one millisecond clock
/// (a timestamp earlier than the latest one seen is read as the latest), and
/// shows what comes back. Timestamps are plain numbers, since this is called
/// once per video frame.
#[wasm_bindgen(js_name = LayerDisplay)]
#[derive(Debug)]
pub struct LayerDisplay {
    inner: RustLayerDisplay,
}

#[wasm_bindgen(js_class = LayerDisplay)]
impl LayerDisplay {
    /// A tile that started showing this person at `nowMs`; `lightAvailable`
    /// is whether the person has a light track yet.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(now_ms: f64, light_available: bool) -> Self {
        Self {
            inner: RustLayerDisplay::new(whole_ms(now_ms), light_available),
        }
    }

    /// The full track presented a frame at `nowMs`.
    #[wasm_bindgen(js_name = fullFrame)]
    pub fn full_frame(&mut self, now_ms: f64) -> ShownLayer {
        self.inner.full_frame(whole_ms(now_ms)).into()
    }

    /// The light track presented a frame at `nowMs`.
    #[wasm_bindgen(js_name = lightFrame)]
    pub fn light_frame(&mut self, now_ms: f64) -> ShownLayer {
        self.inner.light_frame(whole_ms(now_ms)).into()
    }

    /// Time has passed with no frame: notices a stall without a frame
    /// arriving.
    pub fn tick(&mut self, now_ms: f64) -> ShownLayer {
        self.inner.tick(whole_ms(now_ms)).into()
    }

    /// The person's light track appeared or went away. Without one the tile
    /// shows the full track, whatever it is doing.
    #[wasm_bindgen(js_name = setLightAvailable)]
    pub fn set_light_available(&mut self, available: bool, now_ms: f64) -> ShownLayer {
        self.inner
            .set_light_available(available, whole_ms(now_ms))
            .into()
    }

    /// The tile could not observe frames until `nowMs` (a hidden page
    /// presents none). Nothing is counted for that time and the tile starts
    /// again on the full track, as a new tile would.
    pub fn resync(&mut self, now_ms: f64) -> ShownLayer {
        self.inner.resync(whole_ms(now_ms)).into()
    }

    /// What the tile shows.
    #[must_use]
    pub fn shown(&self) -> ShownLayer {
        self.inner.shown().into()
    }

    /// Everything counted since the tile started, as
    /// `[lightSwitches, lightMs, stallMaxMs, stallTotalMs]`.
    #[must_use]
    pub fn counters(&self) -> Vec<f64> {
        counters_array(self.inner.counters())
    }

    /// Everything counted since the last drain, in the order of `counters`,
    /// and start a new interval.
    pub fn drain(&mut self) -> Vec<f64> {
        counters_array(self.inner.drain())
    }
}

/// How often a tile calls `LayerDisplay.tick`, milliseconds. Chosen, not
/// measured: a third of {@link layerFullStallMs}.
#[wasm_bindgen(js_name = layerTickMs)]
#[must_use]
pub fn layer_tick_ms() -> u32 {
    small_ms(LAYER_TICK_MS)
}

/// The full track is standing still once it has presented no frame for this
/// long, milliseconds. Chosen, not measured.
#[wasm_bindgen(js_name = layerFullStallMs)]
#[must_use]
pub fn layer_full_stall_ms() -> u32 {
    small_ms(FULL_STALL_MS)
}

/// The light track is playing if it presented a frame within this,
/// milliseconds. Chosen, not measured.
#[wasm_bindgen(js_name = layerLightFreshMs)]
#[must_use]
pub fn layer_light_fresh_ms() -> u32 {
    small_ms(LIGHT_FRESH_MS)
}

/// How long the full track must present frames without a gap before the tile
/// returns to it, milliseconds. Chosen, not measured.
#[wasm_bindgen(js_name = layerFullSteadyMs)]
#[must_use]
pub fn layer_full_steady_ms() -> u32 {
    small_ms(FULL_STEADY_MS)
}

/// How long a full track that has never presented a frame is waited for
/// before a playing light track is shown instead, milliseconds. Chosen, not
/// measured.
#[wasm_bindgen(js_name = layerFirstFrameGraceMs)]
#[must_use]
pub fn layer_first_frame_grace_ms() -> u32 {
    small_ms(FIRST_FRAME_GRACE_MS)
}

/// A threshold as a plain JavaScript number rather than a `BigInt`.
fn small_ms(ms: u64) -> u32 {
    u32::try_from(ms).unwrap_or(u32::MAX)
}

// ----- sound and picture together (ADR-0070, note of 2026-10-12) -----------

/// One of a remote person's three video tracks.
#[wasm_bindgen(js_name = AvTrack)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvTrack {
    /// The main track (stream id `<peer>`).
    Main = 0,
    /// The middle track (`<peer>~mid`).
    Middle = 1,
    /// The light track (`<peer>~light`).
    Light = 2,
}

/// Why the audio receiver's target is what it is.
#[wasm_bindgen(js_name = AvAudioReason)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvAudioReason {
    /// No audio packets arrived; nothing is changed.
    NotFlowing = 0,
    /// The audio flows but reported no delay; nothing is changed.
    NoReading = 1,
    /// This browser cannot set an audio receiver's delay.
    NoSetter = 2,
    /// Nothing is asked of the audio.
    Free = 3,
    /// The sound waits for a picture that needs more delay than it has.
    WaitingForPicture = 4,
    /// The sound waits as long as it may; the picture is left late.
    WaitBounded = 5,
    /// No picture needs the sound to wait any more; the target is coming
    /// down.
    Releasing = 6,
    /// The engine does not follow the audio target.
    HintIgnored = 7,
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
#[wasm_bindgen(js_name = AvVideoReason)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvVideoReason {
    /// No packets arrived; the track is left alone.
    NotFlowing = 0,
    /// The track flows but reported no delay; nothing is changed.
    NoReading = 1,
    /// There is no measured audio delay to follow.
    NoSound = 2,
    /// This browser cannot set a video receiver's delay.
    NoSetter = 3,
    /// The engine synchronises this track itself; it is only observed.
    EngineSynced = 4,
    /// The track began to flow: it starts at the audio's delay.
    Started = 5,
    /// The skew is inside the dead band.
    Aligned = 6,
    /// The target is being raised: the picture waits for the sound.
    Raising = 7,
    /// The target is being lowered towards the sound.
    Lowering = 8,
    /// The target stands and the receiver has not answered it yet.
    Settling = 9,
    /// The engine does not follow the target; the picture stays early.
    HintIgnored = 10,
    /// The track needs more delay than the sound has; the sound waits.
    SoundWaits = 11,
    /// The track is later than the sound and stays so.
    PictureLate = 12,
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

/// What the rule can achieve in a browser, from which receivers' delays it
/// can set.
#[wasm_bindgen(js_name = AvCapability)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvCapability {
    /// Both can be set.
    Full = 0,
    /// Only video can be set: the audio cannot be delayed here, so a picture
    /// that needs more delay than the sound has stays late.
    PictureWaitsOnly = 1,
    /// Only audio can be set: a picture that runs ahead of the sound cannot
    /// be held back here.
    SoundWaitsOnly = 2,
    /// Neither can be set: the skew is measured and nothing is done.
    ObserveOnly = 3,
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

/// A measured delay as whole milliseconds: rounded; `undefined`, `null`
/// and a non-finite number are "not measured"; a negative one reads as 0
/// and the cast saturates on anything too large.
fn measured_ms(ms: Option<f64>) -> Option<u32> {
    let ms = ms.filter(|ms| ms.is_finite())?;
    if ms <= 0.0 {
        return Some(0);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = ms.round() as u32;
    Some(whole)
}

/// Keeps one remote person's sound and picture together in a group call:
/// from what this browser's receivers report each interval, the target
/// playout delay (`RTCRtpReceiver.jitterBufferTarget`) to set on each.
///
/// One per remote person. Every {@link avAlignReadingMs} (any interval up to
/// two seconds works): call `setAudio` and `setVideo` for each track that
/// exists with what its `inbound-rtp` statistics say for the interval, then
/// `read(nowMs)`, then read back what to set with `audioSetMs()` and
/// `videoSetMs(track)`. A track not reported since the last `read` counts
/// as not flowing. Nothing is allocated per call and only the `AvAlign`
/// itself needs `free()`.
#[wasm_bindgen(js_name = AvAlign)]
#[derive(Debug)]
pub struct AvAlign {
    inner: RustAvAlign,
    staged: RustAvReadings,
    plan: Option<RustAvPlan>,
}

impl AvAlign {
    const fn video(&self, track: AvTrack) -> Option<RustAvVideoPlan> {
        match (&self.plan, track) {
            (Some(plan), AvTrack::Main) => Some(plan.main),
            (Some(plan), AvTrack::Middle) => Some(plan.middle),
            (Some(plan), AvTrack::Light) => Some(plan.light),
            (None, _) => None,
        }
    }
}

#[wasm_bindgen(js_class = AvAlign)]
impl AvAlign {
    /// A remote person's alignment in a browser that can (or cannot) set
    /// `jitterBufferTarget` on an audio receiver and on a video receiver.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(audio_settable: bool, video_settable: bool) -> Self {
        Self {
            inner: RustAvAlign::new(audio_settable, video_settable),
            staged: RustAvReadings::default(),
            plan: None,
        }
    }

    /// The audio receiver's interval: whether packets arrived, the average
    /// jitter-buffer delay in milliseconds (`jb_ms`; `undefined` when
    /// nothing left the buffer) and the network-only need (`jb_net_ms`;
    /// `undefined` where the browser reports none).
    #[wasm_bindgen(js_name = setAudio)]
    pub fn set_audio(&mut self, flowing: bool, delay_ms: Option<f64>, network_ms: Option<f64>) {
        self.staged.audio = RustAvAudioReading {
            flowing,
            delay_ms: measured_ms(delay_ms),
            network_ms: measured_ms(network_ms),
        };
    }

    /// One video receiver's interval. `shown` is whether a tile draws this
    /// track now; `engineSynced` is whether the browser synchronises it with
    /// the audio itself (the main track, which shares the audio's stream
    /// id).
    #[wasm_bindgen(js_name = setVideo)]
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn set_video(
        &mut self,
        track: AvTrack,
        flowing: bool,
        shown: bool,
        engine_synced: bool,
        delay_ms: Option<f64>,
        network_ms: Option<f64>,
    ) {
        let reading = RustAvVideoReading {
            flowing,
            shown,
            engine_synced,
            delay_ms: measured_ms(delay_ms),
            network_ms: measured_ms(network_ms),
        };
        match track {
            AvTrack::Main => self.staged.main = reading,
            AvTrack::Middle => self.staged.middle = reading,
            AvTrack::Light => self.staged.light = reading,
        }
    }

    /// Take the interval reported since the last `read`, at `nowMs` (one
    /// millisecond clock, e.g. `performance.now()`), and decide. The answer
    /// is read with the getters below until the next `read`.
    pub fn read(&mut self, now_ms: f64) {
        let readings = std::mem::take(&mut self.staged);
        self.plan = Some(self.inner.read(whole_ms(now_ms), &readings));
    }

    /// The receivers were replaced: nothing has been told to the new ones.
    /// The next `read` starts every track again and tells every receiver
    /// its target.
    #[wasm_bindgen(js_name = receiversReplaced)]
    pub fn receivers_replaced(&mut self) {
        self.inner.receivers_replaced();
        self.plan = None;
    }

    /// What the rule can achieve in this browser.
    #[must_use]
    pub fn capability(&self) -> AvCapability {
        self.inner.capability().into()
    }

    /// The target to set on the audio receiver now, milliseconds;
    /// `undefined` leaves it alone; `0` gives the target back
    /// (`jitterBufferTarget = null`).
    #[wasm_bindgen(js_name = audioSetMs)]
    #[must_use]
    pub fn audio_set_ms(&self) -> Option<u32> {
        self.plan.and_then(|plan| plan.audio.set_ms)
    }

    /// What the audio receiver was last told, milliseconds; `0` when
    /// nothing.
    #[wasm_bindgen(js_name = audioAskedMs)]
    #[must_use]
    pub fn audio_asked_ms(&self) -> u32 {
        self.plan.map_or(0, |plan| plan.audio.asked_ms)
    }

    /// Why the audio target is what it is.
    #[wasm_bindgen(js_name = audioReason)]
    #[must_use]
    pub fn audio_reason(&self) -> AvAudioReason {
        self.plan
            .map_or(AvAudioReason::NotFlowing, |plan| plan.audio.reason.into())
    }

    /// The target to set on a video receiver now, milliseconds; `undefined`
    /// leaves it alone; `0` gives the target back.
    #[wasm_bindgen(js_name = videoSetMs)]
    #[must_use]
    pub fn video_set_ms(&self, track: AvTrack) -> Option<u32> {
        self.video(track).and_then(|plan| plan.set_ms)
    }

    /// What a video receiver was last told, milliseconds; `0` when nothing.
    #[wasm_bindgen(js_name = videoAskedMs)]
    #[must_use]
    pub fn video_asked_ms(&self, track: AvTrack) -> u32 {
        self.video(track).map_or(0, |plan| plan.asked_ms)
    }

    /// A video track's measured delay minus the audio's, milliseconds:
    /// positive when the picture is later than the sound; `undefined` when
    /// either was not measured in the interval.
    #[wasm_bindgen(js_name = videoSkewMs)]
    #[must_use]
    pub fn video_skew_ms(&self, track: AvTrack) -> Option<i32> {
        self.video(track).and_then(|plan| plan.skew_ms)
    }

    /// Why a video target is what it is.
    #[wasm_bindgen(js_name = videoReason)]
    #[must_use]
    pub fn video_reason(&self, track: AvTrack) -> AvVideoReason {
        self.video(track)
            .map_or(AvVideoReason::NotFlowing, |plan| plan.reason.into())
    }
}

/// What the rule can and cannot achieve with the setters a browser has.
#[wasm_bindgen(js_name = avAlignCapability)]
#[must_use]
pub fn av_align_capability(audio_settable: bool, video_settable: bool) -> AvCapability {
    rust_av_align_capability(audio_settable, video_settable).into()
}

/// The word a diagnostic line uses for a capability, the same on every
/// client.
#[wasm_bindgen(js_name = avCapabilityName)]
#[must_use]
pub fn av_capability_name(capability: AvCapability) -> String {
    RustAvCapability::from(capability).name().to_string()
}

/// The word a diagnostic line uses for an audio reason, the same on every
/// client.
#[wasm_bindgen(js_name = avAudioReasonName)]
#[must_use]
pub fn av_audio_reason_name(reason: AvAudioReason) -> String {
    RustAvAudioReason::from(reason).name().to_string()
}

/// The word a diagnostic line uses for a video reason, the same on every
/// client.
#[wasm_bindgen(js_name = avVideoReasonName)]
#[must_use]
pub fn av_video_reason_name(reason: AvVideoReason) -> String {
    RustAvVideoReason::from(reason).name().to_string()
}

/// How often a client reads its receivers and calls `AvAlign.read`,
/// milliseconds. Chosen, not measured.
#[wasm_bindgen(js_name = avAlignReadingMs)]
#[must_use]
pub fn av_align_reading_ms() -> u32 {
    small_ms(AV_READING_MS)
}

/// The skew inside which a track is left as it is, milliseconds. Chosen,
/// not measured.
#[wasm_bindgen(js_name = avAlignDeadBandMs)]
#[must_use]
pub fn av_align_dead_band_ms() -> u32 {
    AV_DEAD_BAND_MS
}

/// The most the sound is asked to wait for a picture above what the audio
/// needs for itself, milliseconds. Chosen, not measured.
#[wasm_bindgen(js_name = avAlignSoundWaitMaxMs)]
#[must_use]
pub fn av_align_sound_wait_max_ms() -> u32 {
    AV_SOUND_WAIT_MAX_MS
}

/// Who is speaking in a group call: the ring on each tile and the one person
/// holding the floor, decided by one rule for the three clients
/// (`docs/ux/group-call-grid.md` sections 5.2 and 7.3).
///
/// One per call. For every person of the roster, at every reading, call
/// `sample` with the level that was read (`undefined` or `null` when it could
/// not be read), or `silence` while they are muted; then `decideFloor` with
/// the roster order, and read `speaking`, `floor` and the floor history back.
/// Levels are the root mean square of decoded samples, `0..1`. Times are
/// plain numbers on one millisecond clock; fractions are dropped and a
/// negative or non-finite time reads as 0.
#[wasm_bindgen(js_name = SpeakingTracker)]
#[derive(Debug, Default)]
pub struct SpeakingTracker {
    inner: RustSpeakingTracker,
}

#[wasm_bindgen(js_class = SpeakingTracker)]
impl SpeakingTracker {
    /// A tracker of a call nobody has been heard in.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Name this device's own person (`undefined` for none). They are
    /// sampled and have a ring like anyone, and never hold the floor,
    /// wherever they stand in the order given to `decideFloor`.
    #[wasm_bindgen(js_name = setLocal)]
    pub fn set_local(&mut self, person: Option<String>) {
        self.inner.set_local(person.as_deref());
    }

    /// One level reading for one person at `nowMs`. No level is "could not
    /// be read": silence for the ring, and nothing for the person's noise
    /// floor. A level that is not a number is silence too.
    pub fn sample(&mut self, person: &str, level: Option<f64>, now_ms: f64) {
        self.inner.sample(person, level, whole_ms(now_ms));
    }

    /// The person is muted: their ring goes off at once and they give up the
    /// floor. What was learned about their room is kept.
    pub fn silence(&mut self, person: &str) {
        self.inner.silence(person);
    }

    /// The person left the call: everything about them is dropped.
    pub fn forget(&mut self, person: &str) {
        self.inner.forget(person);
    }

    /// Forget everyone who is not one of `people`, the roster as it is now.
    #[wasm_bindgen(js_name = keepOnly)]
    pub fn keep_only(&mut self, people: Vec<String>) {
        self.inner.keep_only(&people);
    }

    /// Settle who holds the floor after the samples so far, and return them
    /// (`undefined` for nobody). `order` is the roster in order, which
    /// breaks a tie; this device's own person may be in it and is skipped.
    #[wasm_bindgen(js_name = decideFloor)]
    pub fn decide_floor(&mut self, order: Vec<String>, now_ms: f64) -> Option<String> {
        self.inner
            .decide_floor(&order, whole_ms(now_ms))
            .map(str::to_owned)
    }

    /// Who holds the floor, as last decided. Never this device's own person.
    #[must_use]
    pub fn floor(&self) -> Option<String> {
        self.inner.floor().map(str::to_owned)
    }

    /// Everyone whose ring is on, this device's own person included, sorted
    /// by id.
    #[must_use]
    pub fn speaking(&self) -> Vec<String> {
        self.inner
            .speaking()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// Whether the person's ring is on.
    #[wasm_bindgen(js_name = isSpeaking)]
    #[must_use]
    pub fn is_speaking(&self, person: &str) -> bool {
        self.inner.is_speaking(person)
    }

    /// Everyone who has held the floor, sorted by id. `floorHeldAtMs` is the
    /// same list's times, in the same order.
    #[wasm_bindgen(js_name = floorHolders)]
    #[must_use]
    pub fn floor_holders(&self) -> Vec<String> {
        self.inner
            .floor_history()
            .into_iter()
            .map(|(id, _)| id.to_owned())
            .collect()
    }

    /// When each person of `floorHolders` last held the floor, on the
    /// caller's clock, in that order.
    #[wasm_bindgen(js_name = floorHeldAtMs)]
    #[must_use]
    pub fn floor_held_at_ms(&self) -> Vec<f64> {
        self.inner
            .floor_history()
            .into_iter()
            // A time handed in as a number is far below 2^53 unless it was
            // nonsense to begin with.
            .map(|(_, at)| {
                #[allow(clippy::cast_precision_loss)]
                let at = at as f64;
                at
            })
            .collect()
    }

    /// When the person last held the floor, or `undefined` for someone who
    /// never has.
    #[wasm_bindgen(js_name = lastHeldFloorMs)]
    #[must_use]
    pub fn last_held_floor_ms(&self, person: &str) -> Option<f64> {
        self.inner.last_held_floor_ms(person).map(|at| {
            #[allow(clippy::cast_precision_loss)]
            let at = at as f64;
            at
        })
    }

    /// What counts as sound for the person now, on the scale of the levels
    /// (for the diagnostics line).
    #[must_use]
    pub fn threshold(&self, person: &str) -> f64 {
        self.inner.threshold(person)
    }
}

/// The threshold a person starts with, before anything is known about the
/// noise around their microphone. Chosen, not measured.
#[wasm_bindgen(js_name = speakingLevel)]
#[must_use]
pub fn speaking_level() -> f64 {
    SPEAKING_LEVEL
}

/// The lowest a person's threshold goes. Chosen, not measured.
#[wasm_bindgen(js_name = speakingLevelMin)]
#[must_use]
pub fn speaking_level_min() -> f64 {
    SPEAKING_LEVEL_MIN
}

/// The highest a person's threshold goes. Chosen, not measured.
#[wasm_bindgen(js_name = speakingLevelMax)]
#[must_use]
pub fn speaking_level_max() -> f64 {
    SPEAKING_LEVEL_MAX
}

/// Sound for this long turns the ring on, milliseconds. Chosen, not
/// measured.
#[wasm_bindgen(js_name = speakingRingOnMs)]
#[must_use]
pub fn speaking_ring_on_ms() -> u32 {
    small_ms(RING_ON_MS)
}

/// Silence for this long turns the ring off, milliseconds. Chosen, not
/// measured.
#[wasm_bindgen(js_name = speakingRingOffMs)]
#[must_use]
pub fn speaking_ring_off_ms() -> u32 {
    small_ms(RING_OFF_MS)
}

/// Continuous speech for this long takes the floor, milliseconds. Chosen,
/// not measured.
#[wasm_bindgen(js_name = speakingFloorOnsetMs)]
#[must_use]
pub fn speaking_floor_onset_ms() -> u32 {
    small_ms(FLOOR_ONSET_MS)
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

        let mut a = AvAlign::new(true, true);
        assert_eq!(a.capability(), AvCapability::Full);
        // Before the first read there is nothing to set.
        assert_eq!(a.video_set_ms(AvTrack::Light), None);
        assert_eq!(a.audio_asked_ms(), 0);
        // Light at the 290 ms floor, the sound at 450 (as fractions, the way
        // statistics divide): light starts at the sound's delay; main is
        // observed only; middle was not reported and is not flowing.
        a.set_audio(true, Some(449.6), None);
        a.set_video(AvTrack::Main, true, false, true, Some(490.2), Some(120.0));
        a.set_video(AvTrack::Light, true, true, false, Some(290.4), None);
        a.read(0.0);
        assert_eq!(a.video_set_ms(AvTrack::Light), Some(450));
        assert_eq!(a.video_asked_ms(AvTrack::Light), 450);
        assert_eq!(a.video_skew_ms(AvTrack::Light), Some(-160));
        assert_eq!(a.video_reason(AvTrack::Light), AvVideoReason::Started);
        assert_eq!(a.video_set_ms(AvTrack::Main), None);
        assert_eq!(a.video_skew_ms(AvTrack::Main), Some(40));
        assert_eq!(a.video_reason(AvTrack::Main), AvVideoReason::EngineSynced);
        assert_eq!(a.video_reason(AvTrack::Middle), AvVideoReason::NotFlowing);
        assert_eq!(a.audio_set_ms(), None);
        assert_eq!(a.audio_reason(), AvAudioReason::Free);
        // The sound jumps to 990: 240 ms a second upwards.
        a.set_audio(true, Some(990.0), None);
        a.set_video(AvTrack::Light, true, true, false, Some(450.0), None);
        a.read(1000.7);
        assert_eq!(a.video_set_ms(AvTrack::Light), Some(690));
        assert_eq!(a.video_reason(AvTrack::Light), AvVideoReason::Raising);
        // Nothing reported: nothing flows, nothing is set, nothing forgotten.
        a.read(2000.0);
        assert_eq!(a.video_set_ms(AvTrack::Light), None);
        assert_eq!(a.video_asked_ms(AvTrack::Light), 690);
        assert_eq!(a.audio_reason(), AvAudioReason::NotFlowing);
        // A reading that is not a number is not a measurement.
        a.set_audio(true, Some(f64::NAN), None);
        a.set_video(AvTrack::Light, true, true, false, Some(-3.0), None);
        a.read(3000.0);
        assert_eq!(a.audio_reason(), AvAudioReason::NoReading);
        assert_eq!(a.video_skew_ms(AvTrack::Light), None);
        // New receivers are told again.
        a.receivers_replaced();
        a.set_audio(true, Some(990.0), None);
        a.set_video(AvTrack::Light, true, true, false, Some(690.0), None);
        a.read(4000.0);
        assert_eq!(a.video_set_ms(AvTrack::Light), Some(990));
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
        // The names are the SFU frame's own.
        for quality in [
            TileVideoQuality::Main,
            TileVideoQuality::Middle,
            TileVideoQuality::Light,
        ] {
            assert!(
                lakko_sfu_proto::VideoQuality::from_wire(&tile_video_quality_name(quality))
                    .is_some()
            );
        }
        assert_eq!(tile_video_quality_name(TileVideoQuality::Middle), "middle");
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
        let mut d = LayerDisplay::new(0.0, true);
        for t in (0..=1000).step_by(20) {
            assert_eq!(d.full_frame(f64::from(t)), ShownLayer::Full);
            assert_eq!(d.light_frame(f64::from(t)), ShownLayer::Full);
        }
        for t in (1020..=1280).step_by(20) {
            d.light_frame(f64::from(t));
        }
        // Fractions of a millisecond are dropped: 1299.9 is 1299, one short
        // of the 300 ms without a full frame at which the tile hands over.
        assert_eq!(d.tick(1299.9), ShownLayer::Full);
        assert_eq!(d.tick(1330.5), ShownLayer::Light);
        assert_eq!(d.shown(), ShownLayer::Light);
        assert_eq!(d.counters(), vec![1.0, 0.0, 330.0, 330.0]);
        assert_eq!(d.drain(), vec![1.0, 0.0, 330.0, 330.0]);
        assert_eq!(d.drain(), vec![0.0; 4]);
        assert_eq!(d.set_light_available(false, 1350.0), ShownLayer::Full);
        // Nothing a browser can hand over moves the clock backwards or
        // panics.
        for odd in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -5.0, 1e300] {
            d.tick(odd);
            d.full_frame(odd);
        }
        assert_eq!(d.resync(5000.0), ShownLayer::Full);
    }

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

    #[test]
    fn contact_transitions_cross_the_boundary_intact() {
        use crate::scoped_discovery::{allows_contact, default_visibility_setting};
        let s = default_visibility_setting();
        let chosen = set_contacts_form(&s, ContactsForm::Chosen);
        assert_eq!(chosen.contacts_form(), ContactsForm::Chosen);
        // The memory of the form crosses too.
        let nobody = choose_visibility_mode(&chosen, VisibilityMode::Nobody);
        let back = choose_visibility_mode(&nobody, VisibilityMode::Some);
        assert_eq!(back.contacts_form(), ContactsForm::Chosen);

        let all = [[1u8; 16], [2u8; 16]].concat();
        let excluded = exclude_contact_under_all(&s, &all, &[2; 16]).unwrap();
        assert_eq!(excluded.contacts_form(), ContactsForm::Chosen);
        assert_eq!(excluded.chosen_contacts(), vec![1u8; 16]);
        assert!(!allows_contact(&excluded, &[2; 16]));
        let more = set_contact_chosen(&excluded, &[3; 16], true).unwrap();
        assert_eq!(more.chosen_contacts(), [[1u8; 16], [3u8; 16]].concat());
        assert_eq!(
            set_contact_chosen(&more, &[1; 16], false)
                .unwrap()
                .chosen_contacts(),
            vec![3u8; 16]
        );
        for refused in [
            set_contact_chosen_inner(&s, &[1; 15], true),
            exclude_contact_under_all_inner(&s, &[1; 17], &[1; 16]),
            exclude_contact_under_all_inner(&s, &all, &[]),
        ] {
            let e = refused.unwrap_err();
            assert_eq!(
                (e.property, e.code.as_str()),
                ("visibilityError", "contactIdLength")
            );
        }
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
        let ended = person_info_sections(PersonInfoCapabilities::new(true, true, None));
        assert!(ended.contains(&"delete-chat".to_string()));
        assert!(!ended.contains(&"call".to_string()));
        let removed = person_info_sections(PersonInfoCapabilities::new(false, false, Some(true)));
        assert!(!removed.iter().any(|s| s == "their-contacts-can-find-me"));
        assert!(
            person_info_sections(PersonInfoCapabilities::new(false, true, Some(true)))
                .iter()
                .any(|s| s == "their-contacts-can-find-me")
        );
        assert!(removed.contains(&"add-contact".to_string()));
        assert!(!removed.contains(&"remove-contact".to_string()));
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
        let mut t = SpeakingTracker::new();
        t.set_local(Some("me".into()));
        let order = || vec!["me".to_string(), "a".to_string(), "b".to_string()];
        assert_eq!(t.floor(), None);
        for step in 0..=15_u32 {
            // Fractions of a millisecond are dropped.
            let now = f64::from(step * 100) + 0.9;
            t.sample("me", Some(0.4), now);
            t.sample("b", Some(0.3), now);
            t.sample("a", None, now);
            let floor = t.decide_floor(order(), now);
            assert_eq!(t.is_speaking("b"), step >= 2);
            assert_eq!(floor.as_deref(), (step == 15).then_some("b"));
        }
        assert_eq!(t.speaking(), vec!["b".to_string(), "me".to_string()]);
        assert_eq!(t.floor().as_deref(), Some("b"));
        assert_eq!(t.floor_holders(), vec!["b".to_string()]);
        assert_eq!(t.floor_held_at_ms(), vec![1500.0]);
        assert_eq!(t.last_held_floor_ms("b"), Some(1500.0));
        assert_eq!(t.last_held_floor_ms("me"), None);
        assert!(t.threshold("a").to_bits() == speaking_level().to_bits());
        // A quiet room pulls the threshold down; a mute keeps it.
        for step in 16..=45_u32 {
            t.sample("a", Some(0.002), f64::from(step * 100));
        }
        assert!((t.threshold("a") - 0.006).abs() < 1e-9);
        t.silence("b");
        assert_eq!(t.decide_floor(order(), 4500.0), None);
        assert_eq!(t.speaking(), vec!["me".to_string()]);
        assert_eq!(t.last_held_floor_ms("b"), Some(1500.0));
        t.forget("b");
        assert!(t.floor_holders().is_empty());
        t.keep_only(vec!["a".into()]);
        assert!(t.speaking().is_empty());
        // Nothing a browser can hand over panics.
        for odd in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -5.0, 1e300] {
            t.sample("a", Some(odd), odd);
            t.sample("", None, odd);
            t.decide_floor(Vec::new(), odd);
        }
    }
}
