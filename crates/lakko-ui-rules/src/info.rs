// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! What the name at the top of a conversation opens, and what is on it.
//!
//! `docs/ux/conversation-info.md`. One way in, three destinations chosen by
//! what the conversation IS: a one-to-one opens the person, a group opens the
//! group, a channel opens the channel.
//!
//! ## Why this is here and not three times over
//!
//! It already was three times over, and the three disagreed. Android wrote
//! the rule as a set of enum cases, iOS as a struct of booleans, web as an
//! array of kebab-case strings - one paragraph of a spec, read three times,
//! by three readers who each stopped at a different place:
//!
//! * `Remove contact` was built on web and Android and not on iOS, whose
//!   source carried a comment asserting no client had one.
//! * `Unsubscribe` was built on web and on neither native, whose sources
//!   carried comments asserting the operation did not exist - while both
//!   natives already held the two functions it is made of.
//! * `Delete this chat` reached a one-to-one whose membership had ended only
//!   on Android; on web and iOS it stayed in the group panel that a
//!   one-to-one had just stopped opening, so ending a one-to-one left a
//!   conversation that could never be closed.
//!
//! None of those is a platform difference. Each is the same sentence of the
//! same document, answered differently, and found by a person looking at two
//! phones side by side rather than by any test. So the answer moves here,
//! where one copy exists and every client asks it.
//!
//! What stays on the client is everything this deliberately says nothing
//! about: which words a control carries, which idiom the layer uses, and
//! whether the person may actually do the thing - a capability comes IN as a
//! boolean the client computed, and is never decided here.

/// Which info a conversation's name opens - section 1's table.
///
/// Decided by what the conversation is, never by which control was tapped:
/// there is one way in, and the destination follows from the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InfoTarget {
    /// A conversation between two people opens the other person.
    Person,
    /// A group opens the group.
    Group,
    /// A channel opens the channel.
    Channel,
}

/// One block of an info layer.
///
/// The three targets share a shape - an identifying header, then the actions,
/// then the detail - and differ only in which blocks belong to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InfoSection {
    // A person (section 2.1).
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

    // A group (section 2.2).
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
    /// Make admin and remove admin. Owner-only (ADR-0022).
    Roles,
    /// Hand the group to somebody else (ADR-0045).
    TransferOwnership,
    /// Leave the group.
    LeaveGroup,

    // A channel (section 2.3).
    /// The `@handle` a channel is found by. Produced by no rule since the
    /// ADR-0087 channel info (UX `group-and-channel-privacy.md` 12.1), which
    /// names a channel's `@name` with [`Self::Handle`] as a group's; kept so
    /// the uniffi ordinals of later blocks stay put.
    ChannelHandle,
    /// What the channel says it is.
    ChannelDescription,
    /// How many devices read it. Produced by no rule since the ADR-0087
    /// channel info: the number is the [`Self::Subscribers`] row's.
    SubscriberCount,
    /// The publisher's own screen. Produced by no rule: the Manage page is
    /// gone (ADR-0087 section 12), its cards are rows of the info screen.
    ManageChannel,
    /// Stop following a channel.
    Unsubscribe,
    /// The channel's member list, shown to its members like a group's
    /// roster (ADR-0060 section 3, `docs/ux/who-can-find-me.md` section
    /// 6.3). Produced by no rule since the ADR-0087 channel info: the list
    /// opens from the [`Self::Subscribers`] row (UX
    /// `group-and-channel-privacy.md` 15.1). Kept so the uniffi ordinals of
    /// later blocks stay put.
    ChannelMembers,
    /// Put a person back on this device's contact list: the one-to-one
    /// outlived the contact, because removing a contact leaves the
    /// conversation (`docs/ux/conversation-info.md` section 2.1). Local, sends
    /// nothing. Appended last for the same reason as [`Self::ChannelMembers`].
    AddContact,
    /// `Their contacts can find me`, the per-contact switch of "Who can find
    /// me" (`docs/ux/who-can-find-me.md` section 5.1), shown only while
    /// Contacts of my contacts is on. Appended last for the same reason as
    /// [`Self::ChannelMembers`].
    TheirContactsCanFindMe,
    /// The `@name` of a group or channel, with a copy control (ADR-0087,
    /// `docs/ux/group-and-channel-privacy.md` section 4). Shown to everyone
    /// in the conversation while it holds a name. Appended last, like every
    /// block below, so earlier uniffi ordinals stay put.
    Handle,
    /// `Who can join` / `Who can subscribe`, opening the type screen (the
    /// flag and the `@name`). Owner only.
    GroupType,
    /// Delete the group or channel: the owner's exit when no one else holds
    /// a role in it (a group of one; a channel with no publisher left).
    DeleteGroup,
    /// Invite people to a channel from contacts (ADR-0087 section 14).
    /// Owner and publishers.
    InvitePeople,
    /// A channel's subscribers: the exact count for the owner and
    /// publishers, the band and the member list for everyone else.
    Subscribers,
    /// A channel's publishers (on screen; co-publishers in the protocol).
    /// Owner, and publishers read-only.
    Admins,
    /// `Reactions`: which reactions the conversation takes, opening the
    /// Reactions screen (`docs/ux/reactions-and-emoji.md` section 9). A
    /// group's owner and admins (the group setting is an owner-or-admin
    /// envelope, checked at receipt like the group name); a channel's owner
    /// (the setting rides the channel metadata record, ADR-0087 section
    /// 13a). Never in a one-to-one, which takes every reaction. Appended last
    /// so earlier uniffi ordinals stay put.
    Reactions,
    /// `Members can find me`, the per-conversation switch of "Who can find
    /// me" (`docs/ux/who-can-find-me.md` section 5), on a channel's info (UX
    /// `group-and-channel-privacy.md` 12.1), shown only while Members of my
    /// groups and channels is on. Appended last so earlier uniffi ordinals
    /// stay put.
    MembersCanFindMe,
    /// Block this person (ADR-0098 section 6, UX `reporting-and-moderation.md`
    /// section 1.1). A person's card only, in the danger block. Appended last
    /// so earlier uniffi ordinals stay put.
    Block,
    /// Unblock a blocked person, in place of [`Self::Block`].
    Unblock,
    /// Report to Lakko's moderators (ADR-0098 section 2, UX section 2.1): on
    /// a person's card, a group's info and a channel's info, never on the
    /// viewer's own group or channel.
    Report,
}

impl InfoSection {
    /// Whether this block is about a GROUP, and so must never appear on a
    /// person.
    ///
    /// Section 2.1's `Never` list - a group name, an invite link, roles,
    /// ownership or `Leave group` - as a property of each block rather than
    /// as a list restated wherever it is needed. A group block added later
    /// inherits the rule instead of having to be remembered into it, which is
    /// the difference between a requirement and a habit.
    ///
    /// `MemberList` and `AddMember` are on the list for the same reason:
    /// a one-to-one has no roster to show, and adding somebody to it starts a
    /// new group rather than growing this one (the maintainer's call,
    /// 2026-09-23).
    #[must_use]
    ///
    /// The ADR-0087 blocks ([`Self::Handle`] onward) are on the list too:
    /// each is about a group or a channel, and none belongs on a person. So
    /// read the name as "conversation-only".
    pub const fn group_only(self) -> bool {
        matches!(
            self,
            Self::GroupName
                | Self::RenameGroup
                | Self::MemberList
                | Self::AddMember
                | Self::InviteLink
                | Self::Roles
                | Self::TransferOwnership
                | Self::LeaveGroup
                | Self::Handle
                | Self::GroupType
                | Self::DeleteGroup
                | Self::InvitePeople
                | Self::Subscribers
                | Self::Admins
                | Self::Reactions
                | Self::MembersCanFindMe
        )
    }

    /// The block's name in kebab case.
    ///
    /// Here rather than on the web client because web's section type has
    /// always been a union of these strings, and a client re-spelling the
    /// names it was handed is the drift this module exists to end.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Call => "call",
            Self::SafetyNumber => "safety-number",
            Self::RenameContact => "rename-contact",
            Self::RemoveContact => "remove-contact",
            Self::DeleteChat => "delete-chat",
            Self::GroupName => "group-name",
            Self::RenameGroup => "rename-group",
            Self::MemberList => "members",
            Self::AddMember => "add-member",
            Self::InviteLink => "invite-link",
            Self::Roles => "roles",
            Self::TransferOwnership => "transfer-ownership",
            Self::LeaveGroup => "leave-group",
            Self::ChannelHandle => "channel-handle",
            Self::ChannelDescription => "channel-description",
            Self::SubscriberCount => "subscriber-count",
            Self::ManageChannel => "manage-channel",
            Self::Unsubscribe => "unsubscribe",
            Self::ChannelMembers => "channel-members",
            Self::AddContact => "add-contact",
            Self::TheirContactsCanFindMe => "their-contacts-can-find-me",
            Self::Handle => "handle",
            Self::GroupType => "group-type",
            Self::DeleteGroup => "delete-group",
            Self::InvitePeople => "invite-people",
            Self::Subscribers => "subscribers",
            Self::Admins => "admins",
            Self::Reactions => "reactions",
            Self::MembersCanFindMe => "members-can-find-me",
            Self::Block => "block",
            Self::Unblock => "unblock",
            Self::Report => "report",
        }
    }
}

/// What the viewer is to a one-to-one conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PersonInfoCapabilities {
    /// This device's membership in the conversation is over (ADR-0045 UX
    /// 6.2).
    ///
    pub membership_ended: bool,
    /// The person is on this device's contact list.
    ///
    /// Every client adopts a one-to-one peer as a contact, except a person
    /// the viewer removed: removing a contact leaves the conversation, and the
    /// removal stays (`docs/ux/conversation-info.md` section 2.1). That
    /// conversation is the one card a person can be opened from while not a
    /// contact, and it offers [`InfoSection::AddContact`] in place of the
    /// rename and remove blocks, which need a contact row to act on.
    pub is_contact: bool,
    /// Contacts of my contacts is on in "Who can find me"
    /// (`docs/ux/who-can-find-me.md` section 5.1), under either form. The
    /// client reads it from the setting, so this crate never does.
    pub contacts_of_contacts_on: bool,
    /// The viewer blocked this person (ADR-0098 section 6). Read from the
    /// account's synced block register.
    pub is_blocked: bool,
}

/// What the viewer may do in a group.
///
/// Every field is a decision the client already made - it reads the live role
/// table and its own membership - and is never re-derived here.
///
/// Five booleans, deliberately: each names a distinct thing the viewer may or
/// may not do, and folding them into a role enum would put the ADR-0022 role
/// table's reading in two places, which is what this crate exists to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Whether this device can actually leave.
    ///
    /// ADR-0045's member-facing exit is a `Leave` PROPOSAL, and a group with
    /// no ADR-0022 role table has no proposal path - OpenMLS refuses a member
    /// committing its own removal, so there is nothing for the control to do.
    /// A client also reads `false` here when it cannot tell which leaf is its
    /// own, because a torn roster read is not a licence to offer an exit.
    ///
    /// It is a field rather than something inferred from the others because
    /// it cannot be inferred: a plain member of a modern group and a plain
    /// member of a legacy one are identical on every other field, and only
    /// one of them can leave. This field exists because the rule got that
    /// wrong first - it offered `Leave group` unconditionally, and BOTH web
    /// and Android had to override it in their own view, which is a rule
    /// nobody is following.
    pub can_leave: bool,
    /// This device's membership is over: read-only (ADR-0045 UX 6.2).
    pub membership_ended: bool,
    /// The group holds an `@name` (ADR-0087). A group is reported by its
    /// `@name`; one without a name is named on no server, so it is not
    /// reported as a group and its members' messages are reported instead
    /// (ADR-0098 open question 3).
    pub has_handle: bool,
}

/// What the viewer is to a channel (ADR-0087 section 15). One role, never
/// two: a publisher reads the channel from its publisher keys, so it is not
/// also a subscriber here even when it subscribed before it was appointed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelRole {
    /// The channel's owner.
    Owner,
    /// A publisher (a co-publisher in the protocol): posts and moderates as
    /// the owner does, and does not rename, change the type or appoint.
    Publisher,
    /// Reads the channel.
    Subscriber,
}

/// What the viewer is to a channel and what the channel info depends on.
///
/// Every field is the client's reading, never re-derived here: the role from
/// the keys this device holds and the appointment, the `@name` from the
/// channel's record, the publishers from the PublisherGroup, the setting from
/// "Who can find me".
///
/// Whether the channel is public or private changes no block of this screen
/// (UX `group-and-channel-privacy.md` 12.1, 14.2: a public channel has an
/// invite link too); it changes the header line and rows inside the
/// Subscribers screen, which the client draws. So it is not an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelInfoCapabilities {
    /// The viewer's role.
    pub role: ChannelRole,
    /// The channel holds an `@name`. A lapsed one shows no row (UX 4.4).
    pub has_handle: bool,
    /// A publisher besides the owner remains. The owner deletes a channel
    /// only when none does (ADR-0087 section 15); a channel's ownership is
    /// not transferred (UX section 19), so an owner with publishers has no
    /// exit on this screen.
    pub has_other_publishers: bool,
    /// Members of my groups and channels is on in "Who can find me"
    /// (`docs/ux/who-can-find-me.md` section 5), under either form. The
    /// client reads it from the setting, so this crate never does.
    pub members_of_groups_and_channels_on: bool,
}

/// Which info the header opens for a conversation of this kind.
///
/// Total over the three kinds the merged list holds, so a client routes a tap
/// by asking rather than by branching on a kind it read itself.
#[must_use]
pub const fn info_target(kind: crate::conversation::ConversationKind) -> InfoTarget {
    match kind {
        crate::conversation::ConversationKind::Chat => InfoTarget::Person,
        crate::conversation::ConversationKind::Group => InfoTarget::Group,
        crate::conversation::ConversationKind::Channel => InfoTarget::Channel,
    }
}

/// What a person's info holds - section 2.1.
///
/// The safety number is always there for a contact: it is the one security
/// thing a person acts on, so it belongs in front of them. Rename and remove
/// need a contact record to act on, so a conversation that outlived its
/// contact - which removing one deliberately leaves behind - offers neither.
///
/// A membership that has ended turns the set over rather than dimming it.
/// `Call` goes, because the per-call key comes from a group this device is no
/// longer in and a call control would assert a live session there is not; and
/// `DeleteChat` arrives, because closing the door is the one thing still open
/// (ADR-0045 UX 6.2.4). Everything about the PERSON - their name, their
/// number, your name for them - is unaffected: the conversation ended, they
/// did not.
///
/// [`InfoSection::TheirContactsCanFindMe`] is about a contact and about a
/// setting, so it needs both: when the setting does not include contacts of
/// contacts the row is absent rather than dead, and a person who is not a
/// contact has no list to be on.
///
/// Nothing in the result is ever [`InfoSection::group_only`]. That is the
/// whole point of the split, and it is asserted rather than described.
#[must_use]
pub fn person_info_sections(caps: PersonInfoCapabilities) -> Vec<InfoSection> {
    let mut sections = Vec::new();
    if caps.membership_ended {
        sections.push(InfoSection::DeleteChat);
    } else if !caps.is_blocked {
        // A blocked person is not called: their calls are declined without
        // a ring, and a call from this side would reach them.
        sections.push(InfoSection::Call);
    }
    // One card, wherever it was opened from. `Message` used to be withheld
    // when the card was reached from the conversation's own header, which
    // needed an origin to be passed in and gave the same person two cards
    // (seen side by side on 2026-09-26). From the conversation the control
    // simply returns to it.
    sections.push(InfoSection::Message);
    sections.push(InfoSection::SafetyNumber);
    if caps.is_contact {
        if caps.contacts_of_contacts_on {
            sections.push(InfoSection::TheirContactsCanFindMe);
        }
        sections.push(InfoSection::RenameContact);
    } else if !caps.is_blocked {
        // Blocking takes the person off the contact list; adding them back
        // waits for an unblock (UX `reporting-and-moderation.md` 1.3).
        sections.push(InfoSection::AddContact);
    }
    // The danger block: report, block, then remove (UX
    // `reporting-and-moderation.md` 1.1, 2.1). A conversation this device is
    // out of is not reported from.
    if !caps.membership_ended {
        sections.push(InfoSection::Report);
    }
    sections.push(if caps.is_blocked {
        InfoSection::Unblock
    } else {
        InfoSection::Block
    });
    if caps.is_contact {
        sections.push(InfoSection::RemoveContact);
    }
    sections
}

/// What a group's info holds - section 2.2.
///
/// An ended membership leaves the name and the roster readable and takes
/// every action away: the conversation is over, the record of who was in it
/// is not. What is left in its place is the one thing the reader still can
/// do, closing the door behind them - which is the same `DeleteChat` a
/// one-to-one gets, in the same state, for the same reason.
#[must_use]
pub fn group_info_sections(caps: GroupInfoCapabilities) -> Vec<InfoSection> {
    let mut sections = vec![InfoSection::GroupName, InfoSection::MemberList];
    if caps.membership_ended {
        sections.push(InfoSection::DeleteChat);
        return sections;
    }
    if caps.can_rename {
        sections.push(InfoSection::RenameGroup);
    }
    if caps.can_manage_members {
        sections.push(InfoSection::AddMember);
    }
    // Owner, not owner-or-admin. ADR-0022 makes a role-table change
    // owner-only, and the peers enforce it: an admin's role commit is refused
    // by every member. Offering the control to an admin would be offering a
    // button whose every press is rejected - and the first draft of this rule
    // did exactly that, by folding roles in with adding members because both
    // are "managing people".
    if caps.is_owner {
        sections.push(InfoSection::Roles);
    }
    if caps.can_mint_invite {
        sections.push(InfoSection::InviteLink);
    }
    // Owner or admin, and still a member: the same authority as a rename
    // (both are owner-or-admin group envelopes checked at receipt).
    if caps.can_rename {
        sections.push(InfoSection::Reactions);
    }
    if caps.is_owner {
        sections.push(InfoSection::TransferOwnership);
    }
    // Report, for anyone but the owner: a group is never reported by the
    // person who runs it (UX `reporting-and-moderation.md` 2.1). Only a group
    // with an `@name`: nothing else names a group to the moderators.
    if !caps.is_owner && caps.has_handle {
        sections.push(InfoSection::Report);
    }
    if caps.can_leave {
        sections.push(InfoSection::LeaveGroup);
    }
    sections
}

/// What a channel's info holds - UX `group-and-channel-privacy.md` 12.1,
/// with sections 4, 14 and 15 and ADR-0087 section 15.
///
/// The order is a group's: the header (`@name`, description, and the owner's
/// in-place rename), the actions and detail, then the exit block.
///
/// - `Invite people`, `Invite link` and `Publishers` are the owner's and the
///   publishers' (a publisher's `Publishers` is read-only; the screen says
///   so, this rule does not).
/// - `Subscribers` is everyone's: the exact count to the owner and the
///   publishers, the band and the shared names to a subscriber. Which of the
///   two is the client's to draw.
/// - Rename, `Reactions` and `Channel type` are the owner's alone: the
///   metadata record carries them and only the owner re-issues it.
/// - `Members can find me` follows the setting, for every role.
/// - The exit: `Unsubscribe` for a subscriber; `Delete channel` for the owner
///   when no publisher remains; nothing for a publisher (stepping down is on
///   its own row of the Publishers list) or for an owner with publishers.
///
/// The Manage page is gone (ADR-0087 section 12): no block here opens it.
#[must_use]
pub fn channel_info_sections(caps: ChannelInfoCapabilities) -> Vec<InfoSection> {
    let owner = caps.role == ChannelRole::Owner;
    let publishes = matches!(caps.role, ChannelRole::Owner | ChannelRole::Publisher);
    let mut sections = Vec::new();
    // The header.
    if caps.has_handle {
        sections.push(InfoSection::Handle);
    }
    sections.push(InfoSection::ChannelDescription);
    // Rename and the description, edited in place by the owner.
    if owner {
        sections.push(InfoSection::RenameGroup);
    }
    // Actions and detail.
    if publishes {
        sections.push(InfoSection::InvitePeople);
        sections.push(InfoSection::InviteLink);
    }
    sections.push(InfoSection::Subscribers);
    if publishes {
        sections.push(InfoSection::Admins);
    }
    if owner {
        sections.push(InfoSection::Reactions);
        sections.push(InfoSection::GroupType);
    }
    if caps.members_of_groups_and_channels_on {
        sections.push(InfoSection::MembersCanFindMe);
    }
    // The exit block.
    match caps.role {
        ChannelRole::Owner => {
            if !caps.has_other_publishers {
                sections.push(InfoSection::DeleteGroup);
            }
        },
        ChannelRole::Publisher => {},
        ChannelRole::Subscriber => {
            // Report is a subscriber's: the owner and the publishers run
            // the channel (UX `reporting-and-moderation.md` 2.1).
            sections.push(InfoSection::Report);
            sections.push(InfoSection::Unsubscribe);
        },
    }
    sections
}

/// Group or channel, for [`conversation_info_sections`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationInfoKind {
    /// A group.
    Group,
    /// A channel.
    Channel,
}

/// The viewer's role in a group or channel (ADR-0022; a channel's
/// co-publisher is its admin, "publisher" on screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationRole {
    /// The owner.
    Owner,
    /// An admin of a group, a publisher of a channel.
    Admin,
    /// A member of a group, a subscriber of a channel.
    Member,
}

/// What the viewer is and may do in a group or channel: ONE record for both
/// kinds (`docs/ux/group-and-channel-privacy.md` section 18), so a channel
/// cannot grow a screen of its own. Every field is the client's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ConversationInfoCapabilities {
    /// Group or channel.
    pub kind: ConversationInfoKind,
    /// The viewer's role.
    pub role: ConversationRole,
    /// Nobody but the owner holds a place that blocks deleting: a group of
    /// one person (ADR-0087 section 10, distinct accounts, not devices); a
    /// channel with no publisher left besides the owner.
    pub alone: bool,
    /// The owner may hand the group over now (ADR-0045: a role-table group
    /// with another member to hand it to).
    pub can_transfer: bool,
    /// This device can leave (see [`GroupInfoCapabilities::can_leave`]).
    pub can_leave: bool,
    /// The conversation holds an `@name` (a lapsed one shows no row,
    /// UX section 4.4).
    pub has_handle: bool,
}

/// What a group's or a channel's info holds under ADR-0087
/// (`docs/ux/group-and-channel-privacy.md` sections 4, 10, 11, 12.1 and 15).
///
/// Replaces [`group_info_sections`] once the clients move to it; that stays
/// until then, because the clients call it. A channel's blocks are
/// [`channel_info_sections`]'s, which this calls, so the two cannot disagree.
/// What differs from the old group rule, on purpose:
///
/// - No `DeleteChat` and no ended-membership state: the end of a membership
///   wipes the conversation (ADR-0087 section 9), so there is no ended
///   group to show.
/// - The owner's exit is [`InfoSection::DeleteGroup`] when alone, else
///   [`InfoSection::TransferOwnership`]; an owner is never offered
///   `LeaveGroup` (UX sections 10 and 11).
/// - A channel shows the group's blocks where they mean the same (rename,
///   invite link) and its own (`InvitePeople`, `Subscribers`, `Admins`);
///   the Manage page is gone (ADR-0087 section 12). This record carries no
///   "Who can find me" setting, so a channel read through it never gets
///   [`InfoSection::MembersCanFindMe`]; call [`channel_info_sections`] for
///   that.
#[must_use]
pub fn conversation_info_sections(caps: ConversationInfoCapabilities) -> Vec<InfoSection> {
    if caps.kind == ConversationInfoKind::Channel {
        // One copy of the channel rule: this record is read as a channel's
        // (admin = publisher, member = subscriber, alone = no publisher left),
        // without the "Who can find me" setting it does not carry.
        return channel_info_sections(ChannelInfoCapabilities {
            role: match caps.role {
                ConversationRole::Owner => ChannelRole::Owner,
                ConversationRole::Admin => ChannelRole::Publisher,
                ConversationRole::Member => ChannelRole::Subscriber,
            },
            has_handle: caps.has_handle,
            has_other_publishers: !caps.alone,
            members_of_groups_and_channels_on: false,
        });
    }
    let owner = caps.role == ConversationRole::Owner;
    let manager = matches!(caps.role, ConversationRole::Owner | ConversationRole::Admin);
    let mut sections = Vec::new();
    if caps.has_handle {
        sections.push(InfoSection::Handle);
    }
    sections.push(InfoSection::GroupName);
    sections.push(InfoSection::MemberList);
    if manager {
        sections.push(InfoSection::RenameGroup);
        sections.push(InfoSection::AddMember);
    }
    // Owner only: peers refuse an admin's role commit (ADR-0022).
    if owner {
        sections.push(InfoSection::Roles);
    }
    if manager {
        sections.push(InfoSection::InviteLink);
        // Owner and admins choose a group's reactions.
        sections.push(InfoSection::Reactions);
    }
    if owner {
        sections.push(InfoSection::GroupType);
    }
    // The exit block. Report first, for anyone but the owner, and only by
    // the group's `@name`.
    if !owner && caps.has_handle {
        sections.push(InfoSection::Report);
    }
    if owner {
        if caps.alone {
            sections.push(InfoSection::DeleteGroup);
        } else if caps.can_transfer {
            sections.push(InfoSection::TransferOwnership);
        }
    } else if caps.can_leave {
        sections.push(InfoSection::LeaveGroup);
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationKind;

    fn person(ended: bool) -> PersonInfoCapabilities {
        PersonInfoCapabilities {
            membership_ended: ended,
            is_contact: true,
            contacts_of_contacts_on: false,
            is_blocked: false,
        }
    }

    fn removed_person(ended: bool) -> PersonInfoCapabilities {
        PersonInfoCapabilities {
            membership_ended: ended,
            is_contact: false,
            contacts_of_contacts_on: false,
            is_blocked: false,
        }
    }

    fn group(ended: bool) -> GroupInfoCapabilities {
        GroupInfoCapabilities {
            can_manage_members: true,
            can_rename: true,
            can_mint_invite: true,
            is_owner: true,
            can_leave: true,
            membership_ended: ended,
            has_handle: false,
        }
    }

    #[test]
    fn the_destination_follows_from_the_conversation() {
        assert_eq!(info_target(ConversationKind::Chat), InfoTarget::Person);
        assert_eq!(info_target(ConversationKind::Group), InfoTarget::Group);
        assert_eq!(info_target(ConversationKind::Channel), InfoTarget::Channel);
    }

    /// Section 2.1's `Never` list, under every combination rather than for
    /// one example - this is the defect the whole module exists to close, so
    /// it is checked exhaustively.
    #[test]
    fn a_person_never_carries_anything_group_shaped() {
        for caps in [
            person(false),
            person(true),
            removed_person(false),
            removed_person(true),
        ] {
            let ended = caps.membership_ended;
            let sections = person_info_sections(caps);
            assert!(
                !sections.iter().any(|s| s.group_only()),
                "ended={ended} -> {sections:?}"
            );
        }
    }

    #[test]
    fn a_contact_gets_the_number_the_name_and_the_way_out_of_the_list() {
        let sections = person_info_sections(person(false));
        assert!(sections.contains(&InfoSection::SafetyNumber));
        assert!(sections.contains(&InfoSection::RenameContact));
        assert!(sections.contains(&InfoSection::RemoveContact));
    }

    /// A contact's card holds the three contact blocks, live or ended.
    #[test]
    fn a_contact_always_gets_the_contact_blocks() {
        for &ended in &[false, true] {
            let sections = person_info_sections(person(ended));
            assert!(sections.contains(&InfoSection::SafetyNumber));
            assert!(sections.contains(&InfoSection::RenameContact));
            assert!(sections.contains(&InfoSection::RemoveContact));
        }
    }

    /// A person the viewer removed keeps the conversation, and its card puts
    /// them back: `Add to contacts` in place of rename and remove, which need
    /// a contact row. The number, the conversation and the call are the
    /// person's, not the contact row's, so they stay.
    #[test]
    fn a_removed_person_is_offered_add_to_contacts_and_nothing_that_needs_a_row() {
        for &ended in &[false, true] {
            let sections = person_info_sections(removed_person(ended));
            assert!(sections.contains(&InfoSection::AddContact), "{sections:?}");
            assert!(sections.contains(&InfoSection::SafetyNumber));
            assert!(sections.contains(&InfoSection::Message));
            assert!(!sections.contains(&InfoSection::RenameContact));
            assert!(!sections.contains(&InfoSection::RemoveContact));
        }
        assert!(person_info_sections(removed_person(false)).contains(&InfoSection::Call));
        assert!(!person_info_sections(person(false)).contains(&InfoSection::AddContact));
    }

    /// The danger block of a person's card (UX `reporting-and-moderation.md`
    /// 1.1, 1.3, 2.1): report, then block or unblock, then remove. A blocked
    /// person is not called and not offered as a contact again until
    /// unblocked; an ended conversation is not reported from.
    #[test]
    fn a_persons_card_reports_blocks_and_unblocks() {
        let contact = PersonInfoCapabilities {
            membership_ended: false,
            is_contact: true,
            contacts_of_contacts_on: false,
            is_blocked: false,
        };
        assert_eq!(
            person_info_sections(contact),
            vec![
                InfoSection::Call,
                InfoSection::Message,
                InfoSection::SafetyNumber,
                InfoSection::RenameContact,
                InfoSection::Report,
                InfoSection::Block,
                InfoSection::RemoveContact,
            ]
        );
        let blocked = PersonInfoCapabilities {
            is_contact: false,
            is_blocked: true,
            ..contact
        };
        assert_eq!(
            person_info_sections(blocked),
            vec![
                InfoSection::Message,
                InfoSection::SafetyNumber,
                InfoSection::Report,
                InfoSection::Unblock,
            ]
        );
        let ended = PersonInfoCapabilities {
            membership_ended: true,
            ..contact
        };
        let s = person_info_sections(ended);
        assert!(!s.contains(&InfoSection::Report));
        assert!(s.contains(&InfoSection::Block));
        for b in [
            InfoSection::Block,
            InfoSection::Unblock,
            InfoSection::Report,
        ] {
            assert!(!b.group_only());
        }
    }

    /// A group is reported by anyone in it but its owner, by its `@name`: a
    /// group with no name is not reported as a group (ADR-0098 open
    /// question 3).
    #[test]
    fn a_group_is_reported_by_anyone_but_its_owner_by_its_name() {
        let mut g = group(false);
        g.has_handle = true;
        assert!(!group_info_sections(g).contains(&InfoSection::Report));
        g.is_owner = false;
        assert!(group_info_sections(g).contains(&InfoSection::Report));
        assert!(!group_info_sections(GroupInfoCapabilities {
            has_handle: false,
            ..g
        })
        .contains(&InfoSection::Report));
        assert!(!group_info_sections(GroupInfoCapabilities {
            membership_ended: true,
            ..g
        })
        .contains(&InfoSection::Report));
        for role in [ConversationRole::Admin, ConversationRole::Member] {
            for has_handle in [false, true] {
                assert_eq!(
                    conversation_info_sections(ConversationInfoCapabilities {
                        kind: ConversationInfoKind::Group,
                        role,
                        alone: false,
                        can_transfer: false,
                        can_leave: true,
                        has_handle,
                    })
                    .contains(&InfoSection::Report),
                    has_handle
                );
            }
        }
        assert!(!conversation_info_sections(ConversationInfoCapabilities {
            kind: ConversationInfoKind::Group,
            role: ConversationRole::Owner,
            alone: false,
            can_transfer: true,
            can_leave: false,
            has_handle: true,
        })
        .contains(&InfoSection::Report));
    }

    /// The switch needs the setting to include contacts of contacts AND the
    /// person to be a contact, under either state of the conversation: what
    /// ends is the conversation, not the person.
    #[test]
    fn the_per_contact_switch_follows_the_setting_and_the_contact_row() {
        for &membership_ended in &[false, true] {
            for &is_contact in &[false, true] {
                for &contacts_of_contacts_on in &[false, true] {
                    let sections = person_info_sections(PersonInfoCapabilities {
                        membership_ended,
                        is_contact,
                        contacts_of_contacts_on,
                        is_blocked: false,
                    });
                    assert_eq!(
                        sections.contains(&InfoSection::TheirContactsCanFindMe),
                        is_contact && contacts_of_contacts_on,
                        "{sections:?}"
                    );
                    assert!(!sections.iter().any(|s| s.group_only()), "{sections:?}");
                }
            }
        }
    }

    /// `docs/ux/conversation-info.md` section 2.1 puts it straight after the
    /// safety number and before the contact's own actions.
    #[test]
    fn the_per_contact_switch_sits_after_the_safety_number() {
        let sections = person_info_sections(PersonInfoCapabilities {
            membership_ended: false,
            is_contact: true,
            contacts_of_contacts_on: true,
            is_blocked: false,
        });
        let at = |wanted| sections.iter().position(|s| *s == wanted).unwrap();
        assert_eq!(
            at(InfoSection::TheirContactsCanFindMe),
            at(InfoSection::SafetyNumber) + 1
        );
        assert!(at(InfoSection::TheirContactsCanFindMe) < at(InfoSection::RenameContact));
    }

    /// One card wherever it was opened from: `Message` is always there, and
    /// from the conversation it simply returns to it. Withholding it by origin
    /// gave the same person two cards.
    #[test]
    fn message_is_offered_whatever_the_card_was_opened_from() {
        assert!(person_info_sections(person(false)).contains(&InfoSection::Message));
        assert!(person_info_sections(person(true)).contains(&InfoSection::Message));
    }

    /// The divergence that let a one-to-one end with no way to close it: the
    /// control lived in the group panel, and a one-to-one had just stopped
    /// opening one.
    #[test]
    fn an_ended_conversation_can_always_be_closed_whatever_kind_it_was() {
        for sections in [
            person_info_sections(person(true)),
            person_info_sections(person(true)),
            group_info_sections(group(true)),
        ] {
            assert!(sections.contains(&InfoSection::DeleteChat), "{sections:?}");
        }
    }

    /// The same state takes the call away, on both kinds: the per-call key
    /// comes from a group this device is no longer in.
    #[test]
    fn an_ended_conversation_offers_no_call_and_no_group_action() {
        let p = person_info_sections(person(true));
        assert!(!p.contains(&InfoSection::Call));
        let g = group_info_sections(group(true));
        for gone in [
            InfoSection::AddMember,
            InfoSection::RenameGroup,
            InfoSection::InviteLink,
            InfoSection::TransferOwnership,
            InfoSection::LeaveGroup,
        ] {
            assert!(!g.contains(&gone), "{gone:?} survived an ended membership");
        }
    }

    /// The person is still the person: what ends is the conversation.
    #[test]
    fn an_ended_conversation_still_shows_everything_about_the_person() {
        let sections = person_info_sections(person(true));
        assert!(sections.contains(&InfoSection::SafetyNumber));
        assert!(sections.contains(&InfoSection::RenameContact));
        assert!(sections.contains(&InfoSection::RemoveContact));
    }

    /// ADR-0022 makes a role-table change owner-only and every peer refuses
    /// an admin's, so an admin offered the control would be offered a button
    /// whose every press is rejected.
    #[test]
    fn only_the_owner_is_offered_the_roles_control() {
        let admin = group_info_sections(GroupInfoCapabilities {
            can_manage_members: true,
            can_rename: true,
            can_mint_invite: true,
            is_owner: false,
            can_leave: true,
            membership_ended: false,
            has_handle: false,
        });
        assert!(admin.contains(&InfoSection::AddMember));
        assert!(!admin.contains(&InfoSection::Roles));
        assert!(group_info_sections(group(false)).contains(&InfoSection::Roles));
    }

    #[test]
    fn a_plain_member_gets_the_roster_and_the_exit_and_nothing_else() {
        let sections = group_info_sections(GroupInfoCapabilities {
            can_manage_members: false,
            can_rename: false,
            can_mint_invite: false,
            is_owner: false,
            can_leave: true,
            membership_ended: false,
            has_handle: false,
        });
        assert!(sections.contains(&InfoSection::GroupName));
        assert!(sections.contains(&InfoSection::MemberList));
        assert!(sections.contains(&InfoSection::LeaveGroup));
        for absent in [
            InfoSection::RenameGroup,
            InfoSection::AddMember,
            InfoSection::Roles,
            InfoSection::InviteLink,
            InfoSection::TransferOwnership,
        ] {
            assert!(!sections.contains(&absent), "{absent:?}");
        }
    }

    /// A group with no ADR-0022 role table has no ADR-0045 proposal path, so
    /// a member cannot commit their own removal and there is nothing for
    /// `Leave group` to do. Both web and Android already refused to draw it
    /// there, against a rule that offered it - which is how this field came
    /// to exist.
    #[test]
    fn a_group_nobody_can_leave_is_not_offered_a_way_out() {
        let legacy = group_info_sections(GroupInfoCapabilities {
            can_manage_members: false,
            can_rename: false,
            can_mint_invite: false,
            is_owner: false,
            can_leave: false,
            membership_ended: false,
            has_handle: false,
        });
        assert!(!legacy.contains(&InfoSection::LeaveGroup));
        // Still a group, and everything readable about it stays readable.
        assert!(legacy.contains(&InfoSection::GroupName));
        assert!(legacy.contains(&InfoSection::MemberList));
    }

    /// An ended membership needs no exit, so the field cannot take away the
    /// one control that state exists to offer.
    #[test]
    fn an_ended_group_can_still_be_closed_even_when_it_could_never_be_left() {
        let sections = group_info_sections(GroupInfoCapabilities {
            can_manage_members: false,
            can_rename: false,
            can_mint_invite: false,
            is_owner: false,
            can_leave: false,
            membership_ended: true,
            has_handle: false,
        });
        assert!(sections.contains(&InfoSection::DeleteChat));
        assert!(!sections.contains(&InfoSection::LeaveGroup));
    }

    /// The vacuity guard for [`a_person_never_carries_anything_group_shaped`].
    ///
    /// That property passes trivially if `group_only` ever became all-false,
    /// which is exactly what a careless edit to the `matches!` list looks
    /// like. This asserts the positive side, so the two together say
    /// something. Carried over from the Android client, whose copy of the
    /// rule had this assertion and whose Rust replacement did not.
    #[test]
    fn the_group_only_list_is_not_empty() {
        let marked = [
            InfoSection::GroupName,
            InfoSection::RenameGroup,
            InfoSection::MemberList,
            InfoSection::AddMember,
            InfoSection::InviteLink,
            InfoSection::Roles,
            InfoSection::TransferOwnership,
            InfoSection::LeaveGroup,
            InfoSection::Reactions,
            InfoSection::MembersCanFindMe,
        ];
        for s in marked {
            assert!(s.group_only(), "{s:?} must be classified group-only");
        }
    }

    /// The other half of the ended-membership rule: `DeleteChat` ARRIVES when
    /// a conversation ends, and must not be sitting there before it does.
    /// Deleting a live conversation's transcript is not an action this design
    /// offers, and it is the one control here that destroys user data.
    #[test]
    fn a_live_conversation_never_offers_to_delete_itself() {
        assert!(!person_info_sections(person(false)).contains(&InfoSection::DeleteChat));
        assert!(!group_info_sections(group(false)).contains(&InfoSection::DeleteChat));
    }

    /// The names cross the boundary to web verbatim, so a duplicate would
    /// silently make two blocks one.
    #[test]
    fn every_section_name_is_distinct() {
        let all = [
            InfoSection::Message,
            InfoSection::Call,
            InfoSection::SafetyNumber,
            InfoSection::RenameContact,
            InfoSection::RemoveContact,
            InfoSection::DeleteChat,
            InfoSection::GroupName,
            InfoSection::RenameGroup,
            InfoSection::MemberList,
            InfoSection::AddMember,
            InfoSection::InviteLink,
            InfoSection::Roles,
            InfoSection::TransferOwnership,
            InfoSection::LeaveGroup,
            InfoSection::ChannelHandle,
            InfoSection::ChannelDescription,
            InfoSection::SubscriberCount,
            InfoSection::ManageChannel,
            InfoSection::Unsubscribe,
            InfoSection::ChannelMembers,
            InfoSection::AddContact,
            InfoSection::TheirContactsCanFindMe,
            InfoSection::Handle,
            InfoSection::GroupType,
            InfoSection::DeleteGroup,
            InfoSection::InvitePeople,
            InfoSection::Subscribers,
            InfoSection::Admins,
            InfoSection::Reactions,
            InfoSection::MembersCanFindMe,
        ];
        let mut names: Vec<&str> = all.iter().map(|s| s.name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }

    /// A section is either about a group or not; the list is what section 2.1
    /// forbids, and it must not quietly grow to include a person's own
    /// blocks.
    #[test]
    fn nothing_a_person_needs_is_marked_group_only() {
        for s in [
            InfoSection::Message,
            InfoSection::Call,
            InfoSection::SafetyNumber,
            InfoSection::RenameContact,
            InfoSection::RemoveContact,
            InfoSection::AddContact,
            InfoSection::TheirContactsCanFindMe,
            InfoSection::DeleteChat,
        ] {
            assert!(!s.group_only(), "{s:?}");
        }
    }

    fn caps(
        kind: ConversationInfoKind,
        role: ConversationRole,
        alone: bool,
    ) -> ConversationInfoCapabilities {
        ConversationInfoCapabilities {
            kind,
            role,
            alone,
            can_transfer: true,
            can_leave: true,
            has_handle: true,
        }
    }

    #[test]
    fn a_solo_owner_deletes_and_never_leaves_or_transfers() {
        let s = conversation_info_sections(caps(
            ConversationInfoKind::Group,
            ConversationRole::Owner,
            true,
        ));
        assert!(s.contains(&InfoSection::DeleteGroup));
        assert!(!s.contains(&InfoSection::TransferOwnership));
        assert!(!s.contains(&InfoSection::LeaveGroup));
        assert!(s.contains(&InfoSection::GroupType));
        assert!(s.contains(&InfoSection::AddMember));
        assert!(s.contains(&InfoSection::InviteLink));
    }

    #[test]
    fn an_owner_with_others_transfers_first() {
        let s = conversation_info_sections(caps(
            ConversationInfoKind::Group,
            ConversationRole::Owner,
            false,
        ));
        assert!(s.contains(&InfoSection::TransferOwnership));
        assert!(!s.contains(&InfoSection::DeleteGroup));
        assert!(!s.contains(&InfoSection::LeaveGroup));
    }

    #[test]
    fn a_member_sees_the_name_and_the_way_out_and_nothing_to_manage() {
        let s = conversation_info_sections(caps(
            ConversationInfoKind::Group,
            ConversationRole::Member,
            false,
        ));
        assert_eq!(
            s,
            vec![
                InfoSection::Handle,
                InfoSection::GroupName,
                InfoSection::MemberList,
                InfoSection::Report,
                InfoSection::LeaveGroup
            ]
        );
    }

    #[test]
    fn only_the_owner_changes_the_type() {
        for kind in [ConversationInfoKind::Group, ConversationInfoKind::Channel] {
            for role in [ConversationRole::Admin, ConversationRole::Member] {
                assert!(!conversation_info_sections(caps(kind, role, false))
                    .contains(&InfoSection::GroupType));
            }
            assert!(
                conversation_info_sections(caps(kind, ConversationRole::Owner, false))
                    .contains(&InfoSection::GroupType)
            );
        }
    }

    #[test]
    fn a_channel_has_its_own_blocks_and_no_delete_chat() {
        let owner = conversation_info_sections(caps(
            ConversationInfoKind::Channel,
            ConversationRole::Owner,
            false,
        ));
        for block in [
            InfoSection::Handle,
            InfoSection::ChannelDescription,
            InfoSection::RenameGroup,
            InfoSection::InvitePeople,
            InfoSection::InviteLink,
            InfoSection::Subscribers,
            InfoSection::Admins,
            InfoSection::GroupType,
        ] {
            assert!(owner.contains(&block), "{block:?}");
        }
        assert!(!owner.contains(&InfoSection::ManageChannel));
        let publisher = conversation_info_sections(caps(
            ConversationInfoKind::Channel,
            ConversationRole::Admin,
            false,
        ));
        assert!(!publisher.contains(&InfoSection::RenameGroup));
        assert!(publisher.contains(&InfoSection::InvitePeople));
        assert!(!publisher.contains(&InfoSection::Unsubscribe));
        let subscriber = conversation_info_sections(caps(
            ConversationInfoKind::Channel,
            ConversationRole::Member,
            false,
        ));
        assert_eq!(
            subscriber,
            vec![
                InfoSection::Handle,
                InfoSection::ChannelDescription,
                InfoSection::Subscribers,
                InfoSection::Report,
                InfoSection::Unsubscribe
            ]
        );
        let solo = conversation_info_sections(caps(
            ConversationInfoKind::Channel,
            ConversationRole::Owner,
            true,
        ));
        assert!(solo.contains(&InfoSection::DeleteGroup));
    }

    /// `docs/ux/reactions-and-emoji.md` section 9: a group's owner and
    /// admins, a channel's owner, and nobody else; never on a person, never
    /// once a group membership has ended.
    #[test]
    fn the_reactions_row_is_for_those_who_choose_them() {
        for (kind, role, shown) in [
            (ConversationInfoKind::Group, ConversationRole::Owner, true),
            (ConversationInfoKind::Group, ConversationRole::Admin, true),
            (ConversationInfoKind::Group, ConversationRole::Member, false),
            (ConversationInfoKind::Channel, ConversationRole::Owner, true),
            (
                ConversationInfoKind::Channel,
                ConversationRole::Admin,
                false,
            ),
            (
                ConversationInfoKind::Channel,
                ConversationRole::Member,
                false,
            ),
        ] {
            for alone in [false, true] {
                assert_eq!(
                    conversation_info_sections(caps(kind, role, alone))
                        .contains(&InfoSection::Reactions),
                    shown,
                    "{kind:?} {role:?} alone={alone}"
                );
            }
        }

        assert!(group_info_sections(group(false)).contains(&InfoSection::Reactions));
        let mut member = group(false);
        member.can_manage_members = false;
        member.can_rename = false;
        member.can_mint_invite = false;
        member.is_owner = false;
        assert!(!group_info_sections(member).contains(&InfoSection::Reactions));
        assert!(!group_info_sections(group(true)).contains(&InfoSection::Reactions));

        for ended in [false, true] {
            for is_contact in [false, true] {
                assert!(!person_info_sections(PersonInfoCapabilities {
                    membership_ended: ended,
                    is_contact,
                    contacts_of_contacts_on: true,
                    is_blocked: false,
                })
                .contains(&InfoSection::Reactions));
            }
        }
        assert!(InfoSection::Reactions.group_only());
        assert_eq!(InfoSection::Reactions.name(), "reactions");
    }

    #[test]
    fn no_delete_chat_and_no_handle_row_without_a_handle() {
        for kind in [ConversationInfoKind::Group, ConversationInfoKind::Channel] {
            for role in [
                ConversationRole::Owner,
                ConversationRole::Admin,
                ConversationRole::Member,
            ] {
                for alone in [false, true] {
                    let mut c = caps(kind, role, alone);
                    c.has_handle = false;
                    let s = conversation_info_sections(c);
                    assert!(!s.contains(&InfoSection::DeleteChat));
                    assert!(!s.contains(&InfoSection::Handle));
                    assert!(s.iter().all(|b| b.group_only()
                        || *b == InfoSection::Unsubscribe
                        || *b == InfoSection::Report
                        || *b == InfoSection::ChannelDescription));
                }
            }
        }
    }

    fn channel(role: ChannelRole) -> ChannelInfoCapabilities {
        ChannelInfoCapabilities {
            role,
            has_handle: true,
            has_other_publishers: false,
            members_of_groups_and_channels_on: true,
        }
    }

    const ROLES: [ChannelRole; 3] = [
        ChannelRole::Owner,
        ChannelRole::Publisher,
        ChannelRole::Subscriber,
    ];

    /// Every combination of the record, for the properties that must hold
    /// under all of them.
    fn every_channel_input() -> Vec<ChannelInfoCapabilities> {
        let mut all = Vec::new();
        for role in ROLES {
            for bits in 0..8u8 {
                all.push(ChannelInfoCapabilities {
                    role,
                    has_handle: bits & 1 != 0,
                    has_other_publishers: bits & 2 != 0,
                    members_of_groups_and_channels_on: bits & 4 != 0,
                });
            }
        }
        all
    }

    /// UX `group-and-channel-privacy.md` 12.1, the owner's screen, in order.
    #[test]
    fn the_owner_of_a_channel_sees_the_12_1_screen_in_order() {
        assert_eq!(
            channel_info_sections(channel(ChannelRole::Owner)),
            vec![
                InfoSection::Handle,
                InfoSection::ChannelDescription,
                InfoSection::RenameGroup,
                InfoSection::InvitePeople,
                InfoSection::InviteLink,
                InfoSection::Subscribers,
                InfoSection::Admins,
                InfoSection::Reactions,
                InfoSection::GroupType,
                InfoSection::MembersCanFindMe,
                InfoSection::DeleteGroup,
            ]
        );
    }

    /// A publisher invites and sees the publishers, and does not rename,
    /// change the type or the reactions, or leave from here (section 15).
    #[test]
    fn a_publisher_invites_and_does_not_own() {
        assert_eq!(
            channel_info_sections(channel(ChannelRole::Publisher)),
            vec![
                InfoSection::Handle,
                InfoSection::ChannelDescription,
                InfoSection::InvitePeople,
                InfoSection::InviteLink,
                InfoSection::Subscribers,
                InfoSection::Admins,
                InfoSection::MembersCanFindMe,
            ]
        );
    }

    /// A subscriber reads what the channel is, the subscribers (the band and
    /// the shared names), the switch, and the way out.
    #[test]
    fn a_subscriber_reads_and_unsubscribes() {
        assert_eq!(
            channel_info_sections(channel(ChannelRole::Subscriber)),
            vec![
                InfoSection::Handle,
                InfoSection::ChannelDescription,
                InfoSection::Subscribers,
                InfoSection::MembersCanFindMe,
                InfoSection::Report,
                InfoSection::Unsubscribe,
            ]
        );
    }

    /// The owner deletes only when no publisher remains (ADR-0087 section
    /// 15), and a channel's ownership is not transferred (UX section 19), so
    /// an owner with publishers has no exit here.
    #[test]
    fn the_owner_deletes_only_when_no_publisher_remains() {
        let mut caps = channel(ChannelRole::Owner);
        assert!(channel_info_sections(caps).contains(&InfoSection::DeleteGroup));
        caps.has_other_publishers = true;
        let s = channel_info_sections(caps);
        for gone in [
            InfoSection::DeleteGroup,
            InfoSection::TransferOwnership,
            InfoSection::Unsubscribe,
            InfoSection::LeaveGroup,
        ] {
            assert!(!s.contains(&gone), "{gone:?} in {s:?}");
        }
    }

    /// Who sees which block, under every combination of the record.
    #[test]
    fn each_block_follows_its_role_under_every_input() {
        for caps in every_channel_input() {
            let s = channel_info_sections(caps);
            let owner = caps.role == ChannelRole::Owner;
            let publishes = caps.role != ChannelRole::Subscriber;
            let has = |b| s.contains(&b);
            assert_eq!(has(InfoSection::Handle), caps.has_handle, "{caps:?}");
            assert!(has(InfoSection::ChannelDescription), "{caps:?}");
            assert!(has(InfoSection::Subscribers), "{caps:?}");
            assert_eq!(has(InfoSection::RenameGroup), owner, "{caps:?}");
            assert_eq!(has(InfoSection::Reactions), owner, "{caps:?}");
            assert_eq!(has(InfoSection::GroupType), owner, "{caps:?}");
            assert_eq!(has(InfoSection::InvitePeople), publishes, "{caps:?}");
            assert_eq!(has(InfoSection::InviteLink), publishes, "{caps:?}");
            assert_eq!(has(InfoSection::Admins), publishes, "{caps:?}");
            assert_eq!(
                has(InfoSection::MembersCanFindMe),
                caps.members_of_groups_and_channels_on,
                "{caps:?}"
            );
            assert_eq!(
                has(InfoSection::Unsubscribe),
                caps.role == ChannelRole::Subscriber,
                "{caps:?}"
            );
            assert_eq!(
                has(InfoSection::DeleteGroup),
                owner && !caps.has_other_publishers,
                "{caps:?}"
            );
        }
    }

    /// The Manage page is gone (ADR-0087 section 12), and so are the blocks
    /// the old channel info produced; no person block and no group-only
    /// roster or exit reaches a channel; nothing is listed twice.
    #[test]
    fn a_channel_has_no_manage_page_no_person_blocks_and_no_duplicates() {
        for caps in every_channel_input() {
            let s = channel_info_sections(caps);
            for gone in [
                InfoSection::ManageChannel,
                InfoSection::ChannelHandle,
                InfoSection::SubscriberCount,
                InfoSection::ChannelMembers,
                InfoSection::Message,
                InfoSection::Call,
                InfoSection::SafetyNumber,
                InfoSection::RenameContact,
                InfoSection::RemoveContact,
                InfoSection::AddContact,
                InfoSection::TheirContactsCanFindMe,
                InfoSection::DeleteChat,
                InfoSection::GroupName,
                InfoSection::MemberList,
                InfoSection::AddMember,
                InfoSection::Roles,
                InfoSection::TransferOwnership,
                InfoSection::LeaveGroup,
            ] {
                assert!(!s.contains(&gone), "{gone:?} in {s:?} for {caps:?}");
            }
            let mut sorted = s.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), s.len(), "{s:?}");
        }
    }

    /// Header first, the exit block last, whatever the role.
    #[test]
    fn a_channel_keeps_the_group_order() {
        for caps in every_channel_input() {
            let s = channel_info_sections(caps);
            let at = |b| s.iter().position(|x| *x == b);
            let description = at(InfoSection::ChannelDescription).unwrap();
            if let Some(handle) = at(InfoSection::Handle) {
                assert_eq!(handle, 0);
                assert_eq!(description, 1);
            } else {
                assert_eq!(description, 0);
            }
            let subscribers = at(InfoSection::Subscribers).unwrap();
            for header_or_action in [
                InfoSection::RenameGroup,
                InfoSection::InvitePeople,
                InfoSection::InviteLink,
            ] {
                if let Some(i) = at(header_or_action) {
                    assert!(i < subscribers, "{s:?}");
                }
            }
            for exit in [InfoSection::Unsubscribe, InfoSection::DeleteGroup] {
                if let Some(i) = at(exit) {
                    assert_eq!(i, s.len() - 1, "{s:?}");
                }
            }
            if let Some(i) = at(InfoSection::MembersCanFindMe) {
                assert!(i > subscribers, "{s:?}");
            }
        }
    }

    /// `conversation_info_sections` answers a channel with this rule, so the
    /// two cannot drift.
    #[test]
    fn the_conversation_rule_reads_a_channel_through_the_channel_rule() {
        for (role, channel_role) in [
            (ConversationRole::Owner, ChannelRole::Owner),
            (ConversationRole::Admin, ChannelRole::Publisher),
            (ConversationRole::Member, ChannelRole::Subscriber),
        ] {
            for alone in [false, true] {
                for has_handle in [false, true] {
                    let via_conversation =
                        conversation_info_sections(ConversationInfoCapabilities {
                            kind: ConversationInfoKind::Channel,
                            role,
                            alone,
                            can_transfer: true,
                            can_leave: true,
                            has_handle,
                        });
                    let direct = channel_info_sections(ChannelInfoCapabilities {
                        role: channel_role,
                        has_handle,
                        has_other_publishers: !alone,
                        members_of_groups_and_channels_on: false,
                    });
                    assert_eq!(via_conversation, direct);
                }
            }
        }
    }

    /// The new block's name crosses to web as written.
    #[test]
    fn the_members_switch_is_conversation_only_and_named() {
        assert!(InfoSection::MembersCanFindMe.group_only());
        assert_eq!(InfoSection::MembersCanFindMe.name(), "members-can-find-me");
    }
}
