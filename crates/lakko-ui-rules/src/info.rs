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
    /// The channel's member list, shown to its members like a group's
    /// roster (ADR-0060 section 3, `docs/ux/who-can-find-me.md` section
    /// 6.3). Its own block rather than [`Self::MemberList`], which is
    /// group-only: a channel roster has no roles and no add. Appended last so
    /// the uniffi ordinals of every earlier block stay put.
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
        }
    }
}

/// What the viewer is to a one-to-one conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

/// What the viewer is to a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelInfoCapabilities {
    /// This device holds the publisher keys.
    pub is_publisher: bool,
    /// This device reads the channel as a subscriber.
    pub is_subscriber: bool,
    /// This device has a subscriber count. Only a publisher ever does.
    pub subscriber_count_known: bool,
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
    } else {
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
        sections.push(InfoSection::RemoveContact);
    } else {
        sections.push(InfoSection::AddContact);
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
    if caps.can_leave {
        sections.push(InfoSection::LeaveGroup);
    }
    sections
}

/// What a channel's info holds - section 2.3.
///
/// The subscriber count is offered only when this device has one: only a
/// publisher ever lists subscribers, and a reader inventing a number it
/// cannot know would be worse than showing none.
///
/// `Unsubscribe` is for a subscriber and NOT for a publisher, and that is not
/// symmetry for its own sake: a publisher reads its own channel from its own
/// publisher keys, so there is no subscription to end and a control offering
/// to end one would end nothing.
#[must_use]
pub fn channel_info_sections(caps: ChannelInfoCapabilities) -> Vec<InfoSection> {
    let mut sections = vec![InfoSection::ChannelHandle, InfoSection::ChannelDescription];
    if caps.subscriber_count_known {
        sections.push(InfoSection::SubscriberCount);
    }
    // A channel lists its members to its members (UX 6.3): whoever reads it,
    // as a publisher or a subscriber, holds the epoch seed the member cards
    // are sealed under. A viewer who reads neither cannot open a card.
    if caps.is_publisher || caps.is_subscriber {
        sections.push(InfoSection::ChannelMembers);
    }
    if caps.is_publisher {
        sections.push(InfoSection::ManageChannel);
    } else if caps.is_subscriber {
        sections.push(InfoSection::Unsubscribe);
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
/// Replaces [`group_info_sections`] and [`channel_info_sections`] once the
/// clients move to it; both stay until then, because the clients call them.
/// What differs from them, on purpose:
///
/// - No `DeleteChat` and no ended-membership state: the end of a membership
///   wipes the conversation (ADR-0087 section 9), so there is no ended
///   group to show.
/// - The owner's exit is [`InfoSection::DeleteGroup`] when alone, else
///   [`InfoSection::TransferOwnership`]; an owner is never offered
///   `LeaveGroup` (UX sections 10 and 11).
/// - A channel shows the group's blocks where they mean the same (rename,
///   invite link) and its own (`InvitePeople`, `Subscribers`, `Admins`);
///   the Manage page is gone (ADR-0087 section 12).
#[must_use]
pub fn conversation_info_sections(caps: ConversationInfoCapabilities) -> Vec<InfoSection> {
    let owner = caps.role == ConversationRole::Owner;
    let manager = matches!(caps.role, ConversationRole::Owner | ConversationRole::Admin);
    let mut sections = Vec::new();
    if caps.has_handle {
        sections.push(InfoSection::Handle);
    }
    match caps.kind {
        ConversationInfoKind::Group => {
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
        },
        ConversationInfoKind::Channel => {
            sections.push(InfoSection::ChannelDescription);
            // Rename and the description are the owner's (ADR-0087 section 15).
            if owner {
                sections.push(InfoSection::RenameGroup);
            }
            if manager {
                sections.push(InfoSection::InvitePeople);
                sections.push(InfoSection::InviteLink);
            }
            sections.push(InfoSection::Subscribers);
            if manager {
                sections.push(InfoSection::Admins);
            }
            // The owner's: the setting rides the channel metadata record
            // (ADR-0087 section 13a), which a publisher does not re-issue.
            if owner {
                sections.push(InfoSection::Reactions);
                sections.push(InfoSection::GroupType);
            }
        },
    }
    // The exit block.
    if owner {
        if caps.alone {
            sections.push(InfoSection::DeleteGroup);
        } else if caps.can_transfer && caps.kind == ConversationInfoKind::Group {
            sections.push(InfoSection::TransferOwnership);
        }
    } else {
        match caps.kind {
            ConversationInfoKind::Group => {
                if caps.can_leave {
                    sections.push(InfoSection::LeaveGroup);
                }
            },
            ConversationInfoKind::Channel => {
                if caps.role == ConversationRole::Member {
                    sections.push(InfoSection::Unsubscribe);
                }
            },
        }
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
        }
    }

    fn removed_person(ended: bool) -> PersonInfoCapabilities {
        PersonInfoCapabilities {
            membership_ended: ended,
            is_contact: false,
            contacts_of_contacts_on: false,
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
        });
        assert!(sections.contains(&InfoSection::DeleteChat));
        assert!(!sections.contains(&InfoSection::LeaveGroup));
    }

    /// A publisher has no subscription to end, so offering to end one would
    /// end nothing.
    #[test]
    fn only_a_subscriber_is_offered_a_way_to_stop_following() {
        let subscriber = channel_info_sections(ChannelInfoCapabilities {
            is_publisher: false,
            is_subscriber: true,
            subscriber_count_known: false,
        });
        assert!(subscriber.contains(&InfoSection::Unsubscribe));
        assert!(subscriber.contains(&InfoSection::ChannelMembers));
        assert!(!subscriber.contains(&InfoSection::ManageChannel));
        assert!(!subscriber.contains(&InfoSection::SubscriberCount));

        let publisher = channel_info_sections(ChannelInfoCapabilities {
            is_publisher: true,
            is_subscriber: true,
            subscriber_count_known: true,
        });
        assert!(!publisher.contains(&InfoSection::Unsubscribe));
        assert!(publisher.contains(&InfoSection::ChannelMembers));
        assert!(publisher.contains(&InfoSection::ManageChannel));
        assert!(publisher.contains(&InfoSection::SubscriberCount));
    }

    /// A channel lists its members to its members and to nobody else (UX
    /// 6.3), under every combination; the block sits after what the channel
    /// is and before the reader's own action.
    #[test]
    fn a_channel_shows_its_members_to_its_readers_only() {
        for &is_publisher in &[false, true] {
            for &is_subscriber in &[false, true] {
                for &count in &[false, true] {
                    let sections = channel_info_sections(ChannelInfoCapabilities {
                        is_publisher,
                        is_subscriber,
                        subscriber_count_known: count,
                    });
                    let reads = is_publisher || is_subscriber;
                    assert_eq!(
                        sections.contains(&InfoSection::ChannelMembers),
                        reads,
                        "publisher={is_publisher} subscriber={is_subscriber}"
                    );
                    if let Some(at) = sections
                        .iter()
                        .position(|s| *s == InfoSection::ChannelMembers)
                    {
                        let description = sections
                            .iter()
                            .position(|s| *s == InfoSection::ChannelDescription)
                            .unwrap();
                        assert!(at > description);
                        assert_eq!(at, sections.len() - 2, "{sections:?}");
                    }
                }
            }
        }
        assert!(!InfoSection::ChannelMembers.group_only());
        assert_eq!(InfoSection::ChannelMembers.name(), "channel-members");
    }

    /// A channel that is neither: found by handle, not yet subscribed to.
    #[test]
    fn a_channel_nobody_here_reads_yet_shows_what_it_is_and_no_actions() {
        let sections = channel_info_sections(ChannelInfoCapabilities {
            is_publisher: false,
            is_subscriber: false,
            subscriber_count_known: false,
        });
        assert_eq!(
            sections,
            vec![InfoSection::ChannelHandle, InfoSection::ChannelDescription]
        );
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

    /// A channel is not a group either, under every combination rather than
    /// at one picked example.
    #[test]
    fn a_channel_never_carries_anything_group_shaped() {
        for &is_publisher in &[false, true] {
            for &is_subscriber in &[false, true] {
                for &count in &[false, true] {
                    let sections = channel_info_sections(ChannelInfoCapabilities {
                        is_publisher,
                        is_subscriber,
                        subscriber_count_known: count,
                    });
                    assert!(
                        !sections.iter().any(|s| s.group_only()),
                        "publisher={is_publisher} subscriber={is_subscriber} -> {sections:?}"
                    );
                }
            }
        }
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
                        || *b == InfoSection::ChannelDescription));
                }
            }
        }
    }
}
