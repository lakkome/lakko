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
        }
    }
}

/// Where the info was opened from.
///
/// It decides one thing - whether `Message` is offered. Opened from the
/// conversation, a control that opens the conversation you are already
/// looking at is noise; opened from anywhere else it is the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoOrigin {
    /// From the conversation's own header.
    Conversation,
    /// From the contact list, a search result, a call record.
    Elsewhere,
}

/// What the viewer is to a one-to-one conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonInfoCapabilities {
    /// The other person is in this device's contact list, so the contact
    /// blocks have a record to act on. Since 2026-09-26 every client derives
    /// the contact row from the one-to-one conversation itself
    /// (`docs/ux/conversation-info.md` section 2.1), so from a conversation
    /// this is `true` whenever the roster can be read; `false` survives as
    /// the fallback for a roster that cannot be.
    pub is_contact: bool,
    /// This device's membership in the conversation is over (ADR-0045 UX
    /// 6.2).
    pub membership_ended: bool,
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
/// Nothing in the result is ever [`InfoSection::group_only`]. That is the
/// whole point of the split, and it is asserted rather than described.
#[must_use]
pub fn person_info_sections(origin: InfoOrigin, caps: PersonInfoCapabilities) -> Vec<InfoSection> {
    let mut sections = Vec::new();
    if caps.membership_ended {
        sections.push(InfoSection::DeleteChat);
    } else {
        sections.push(InfoSection::Call);
    }
    if origin == InfoOrigin::Elsewhere {
        sections.push(InfoSection::Message);
    }
    if caps.is_contact {
        sections.push(InfoSection::SafetyNumber);
        sections.push(InfoSection::RenameContact);
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
    if caps.is_publisher {
        sections.push(InfoSection::ManageChannel);
    } else if caps.is_subscriber {
        sections.push(InfoSection::Unsubscribe);
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationKind;

    fn person(is_contact: bool, ended: bool) -> PersonInfoCapabilities {
        PersonInfoCapabilities {
            is_contact,
            membership_ended: ended,
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
        for &origin in &[InfoOrigin::Conversation, InfoOrigin::Elsewhere] {
            for &is_contact in &[false, true] {
                for &ended in &[false, true] {
                    let sections = person_info_sections(origin, person(is_contact, ended));
                    assert!(
                        !sections.iter().any(|s| s.group_only()),
                        "{origin:?} contact={is_contact} ended={ended} -> {sections:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_contact_gets_the_number_the_name_and_the_way_out_of_the_list() {
        let sections = person_info_sections(InfoOrigin::Conversation, person(true, false));
        assert!(sections.contains(&InfoSection::SafetyNumber));
        assert!(sections.contains(&InfoSection::RenameContact));
        assert!(sections.contains(&InfoSection::RemoveContact));
    }

    /// A conversation that outlived its contact record is still a person.
    #[test]
    fn a_person_who_is_not_a_contact_offers_nothing_to_act_on_a_record_with() {
        let sections = person_info_sections(InfoOrigin::Conversation, person(false, false));
        assert!(!sections.contains(&InfoSection::SafetyNumber));
        assert!(!sections.contains(&InfoSection::RenameContact));
        assert!(!sections.contains(&InfoSection::RemoveContact));
        assert!(sections.contains(&InfoSection::Call));
    }

    #[test]
    fn message_is_offered_from_anywhere_but_the_conversation_itself() {
        assert!(
            !person_info_sections(InfoOrigin::Conversation, person(true, false))
                .contains(&InfoSection::Message)
        );
        assert!(
            person_info_sections(InfoOrigin::Elsewhere, person(true, false))
                .contains(&InfoSection::Message)
        );
    }

    /// The divergence that let a one-to-one end with no way to close it: the
    /// control lived in the group panel, and a one-to-one had just stopped
    /// opening one.
    #[test]
    fn an_ended_conversation_can_always_be_closed_whatever_kind_it_was() {
        for sections in [
            person_info_sections(InfoOrigin::Conversation, person(true, true)),
            person_info_sections(InfoOrigin::Elsewhere, person(false, true)),
            group_info_sections(group(true)),
        ] {
            assert!(sections.contains(&InfoSection::DeleteChat), "{sections:?}");
        }
    }

    /// The same state takes the call away, on both kinds: the per-call key
    /// comes from a group this device is no longer in.
    #[test]
    fn an_ended_conversation_offers_no_call_and_no_group_action() {
        let p = person_info_sections(InfoOrigin::Conversation, person(true, true));
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
        let sections = person_info_sections(InfoOrigin::Conversation, person(true, true));
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
        assert!(!subscriber.contains(&InfoSection::ManageChannel));
        assert!(!subscriber.contains(&InfoSection::SubscriberCount));

        let publisher = channel_info_sections(ChannelInfoCapabilities {
            is_publisher: true,
            is_subscriber: true,
            subscriber_count_known: true,
        });
        assert!(!publisher.contains(&InfoSection::Unsubscribe));
        assert!(publisher.contains(&InfoSection::ManageChannel));
        assert!(publisher.contains(&InfoSection::SubscriberCount));
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
        for &origin in &[InfoOrigin::Conversation, InfoOrigin::Elsewhere] {
            for &is_contact in &[false, true] {
                let sections = person_info_sections(origin, person(is_contact, false));
                assert!(
                    !sections.contains(&InfoSection::DeleteChat),
                    "{origin:?} contact={is_contact} -> {sections:?}"
                );
            }
        }
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
            InfoSection::DeleteChat,
        ] {
            assert!(!s.group_only(), "{s:?}");
        }
    }
}
