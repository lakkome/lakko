// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/**
 * What opens when a person taps the name at the top of a conversation —
 * `docs/ux/conversation-info.md`.
 *
 * One entry point, three destinations chosen by what the conversation IS: a
 * one-to-one opens the person, a group opens the group, a channel opens the
 * channel. Before this, one panel did all three jobs with the group-only parts
 * behind a flag, so a chat with one other person opened `Members (2)`, a
 * `Group name` field, `Create invite link`, `Make admin`, `Transfer ownership`
 * and a `Leave group` explaining that ownership must be transferred first —
 * every one of them about a group that does not exist.
 *
 * A flag that hides rows is not the same thing as a screen about a person, and
 * that is why the decision lives here as a value rather than as a boolean the
 * layout reads: the answer to "which info is this" is made once, in one place,
 * and the Compose side renders whichever one it is handed.
 *
 * Pure of Compose and of Android, so the host JVM suite can run it.
 */

/**
 * Where the info was opened from.
 *
 * It decides one thing — whether `Message` is offered. Opened from the
 * conversation, a button that opens the conversation you are already looking at
 * is noise; opened from anywhere else it is the point.
 */
public enum class InfoOrigin { Conversation, Elsewhere }

/** Which info a conversation's header opens. */
public sealed interface ConversationInfo {
    /** A person this device holds as a contact: the full contact screen. */
    public data class Person(val contactKey: String) : ConversationInfo

    /**
     * A person this device does NOT hold as a contact — a chat that outlived
     * its contact row, which removing a contact deliberately leaves behind.
     *
     * It is still a person, so it must not fall back to the group panel; what
     * it shows is what is knowable without a contact record.
     */
    public data object PersonUnknown : ConversationInfo

    /** A group: the roster, the name, the invite link, the roles, the exit. */
    public data object Group : ConversationInfo
}

/**
 * One accepted contact, reduced to the two facts that can tie it to a chat.
 *
 * [routeKey] is the base64url signature key the contact screen is keyed on;
 * [groupIdHex] is the chat this device created with them, when there is one.
 */
public data class ContactLink(
    val routeKey: String,
    val groupIdHex: String?,
)

/**
 * Which info [groupIdHex] opens.
 *
 * The group test is the same one the header's own shape uses: the persisted
 * kind, OR more than one other member — a conversation with three people in it
 * is a group whatever was written down when it had two.
 *
 * A one-to-one is then matched to a contact by the CHAT first and by the
 * signature key second. The chat is the stronger link: a contact row records
 * the conversation this device opened with that person, whereas a leaf
 * signature key names one of their devices (ADR-0050), so a key match can miss
 * a person who is plainly there. The key is kept as the fallback for a contact
 * whose chat was never recorded.
 */
public fun conversationInfo(
    kind: ConversationKind,
    otherMemberCount: Int,
    groupIdHex: String,
    peerRouteKey: String?,
    contacts: List<ContactLink>,
): ConversationInfo {
    if (kind == ConversationKind.Group || otherMemberCount > 1) return ConversationInfo.Group
    contacts.firstOrNull { it.groupIdHex?.equals(groupIdHex, ignoreCase = true) == true }
        ?.let { return ConversationInfo.Person(it.routeKey) }
    if (peerRouteKey != null) {
        contacts.firstOrNull { it.routeKey == peerRouteKey }
            ?.let { return ConversationInfo.Person(it.routeKey) }
    }
    return ConversationInfo.PersonUnknown
}

/**
 * A section of an info layer.
 *
 * [groupOnly] is the classification requirement 2.1 of the spec turns on:
 * "Never: a group name, an invite link, roles, ownership, or `Leave group`."
 * Marking it on the section itself, rather than restating the list wherever it
 * is needed, is what lets a new group section inherit the rule instead of
 * having to be remembered into it.
 */
public enum class InfoSection(public val groupOnly: Boolean = false) {
    // A person.
    Message,
    Call,
    SafetyNumber,
    RenameContact,
    RemoveContact,
    DeleteChat,

    // A group.
    GroupName(groupOnly = true),
    MemberList(groupOnly = true),
    AddMember(groupOnly = true),
    InviteLink(groupOnly = true),
    Roles(groupOnly = true),
    TransferOwnership(groupOnly = true),
    LeaveGroup(groupOnly = true),

    // A channel.
    ChannelHandle,
    ChannelDescription,
    SubscriberCount,
    ManageChannel,
}

/**
 * What a person's info holds — spec section 2.1.
 *
 * The safety number is always there for a contact: it is the one security
 * thing a person acts on, so it belongs in front of them. `Message` depends on
 * where this was opened from. Rename and remove need a contact record to act
 * on, so a chat that outlived its contact offers neither.
 *
 * [membershipEnded] is the state spec section 4 calls read-only, and it turns
 * the set over rather than dimming it: `Call` goes, because the per-call key
 * comes from a group this device is no longer in and a call control asserts a
 * live session there is not; and `DeleteChat` arrives, because closing the
 * door is the one thing still open (ADR-0045 UX 6.2.4), and it used to live in
 * the group panel that a one-to-one no longer opens. Everything about the
 * PERSON — their name, their number, your name for them — is unaffected: the
 * conversation ended, they did not.
 */
public fun personInfoSections(
    origin: InfoOrigin,
    isContact: Boolean,
    membershipEnded: Boolean = false,
): Set<InfoSection> {
    val sections = mutableSetOf<InfoSection>()
    if (membershipEnded) sections += InfoSection.DeleteChat else sections += InfoSection.Call
    if (origin == InfoOrigin.Elsewhere) sections += InfoSection.Message
    if (isContact) {
        sections += InfoSection.SafetyNumber
        sections += InfoSection.RenameContact
        sections += InfoSection.RemoveContact
    }
    return sections
}

/**
 * What a channel's info holds — spec section 2.3.
 *
 * The subscriber count is offered only when this device has one: only a
 * publisher ever lists subscribers, and a reader inventing a number it cannot
 * know would be worse than showing none.
 *
 * `Unsubscribe` is NOT here, and its absence is a statement rather than an
 * oversight: this client has no unsubscribe at all — nothing in the channels
 * client deletes a subscriber registration — so a control here would either do
 * nothing or quietly mean something else.
 */
public fun channelInfoSections(
    isPublisher: Boolean,
    subscriberCountKnown: Boolean,
): Set<InfoSection> {
    val sections = mutableSetOf(InfoSection.ChannelHandle, InfoSection.ChannelDescription)
    if (subscriberCountKnown) sections += InfoSection.SubscriberCount
    if (isPublisher) sections += InfoSection.ManageChannel
    return sections
}
