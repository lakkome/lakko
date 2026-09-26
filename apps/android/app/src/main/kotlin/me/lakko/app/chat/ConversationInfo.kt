// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/**
 * Which of the three info destinations a conversation's header opens —
 * `docs/ux/conversation-info.md` section 1.
 *
 * One entry point, three destinations chosen by what the conversation IS: a
 * one-to-one opens the person, a group opens the group, a channel opens the
 * channel. Before this, one panel did all three jobs with the group-only parts
 * behind a flag, so a chat with one other person opened `Members (2)`, a
 * `Group name` field, `Create invite link`, `Make admin`, `Transfer ownership`
 * and a `Leave group` explaining that ownership must be transferred first —
 * every one of them about a group that does not exist.
 *
 * ## What this file decides, and what it no longer decides
 *
 * It decides **which conversation this is, and whose contact record it
 * belongs to** — the matching rule below. It no longer decides **what the
 * resulting screen holds**: [uniffi.lakko_ffi_uniffi.personInfoSections],
 * [uniffi.lakko_ffi_uniffi.groupInfoSections] and
 * [uniffi.lakko_ffi_uniffi.channelInfoSections] answer that now, from
 * `crates/lakko-ui-rules/src/info.rs`, because the same paragraph of the same
 * spec had been read three times by three clients and answered three
 * different ways.
 *
 * The matching rule stays here because it is not that rule: it reads this
 * device's own contact list and the chat it recorded, neither of which the
 * core holds. It is a candidate for the same treatment later — the shape is
 * the same, a pure function over values a client already has — and moving it
 * would need the contact link modelled across the FFI first.
 *
 * Pure of Compose and of Android, so the host JVM suite can run it.
 */

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
 * HOW the header presents what [conversationInfo] chose —
 * `docs/ux/conversation-info.md` section 3.
 *
 * A group's info is a layer over the conversation, because it has no screen
 * of its own to reuse. A person's info IS the contact screen, and there is one
 * of it: the header pushes the same route the contacts list pushes, with a
 * Back control and the same controls in the same order. Until 2026-09-26 the
 * header hosted that screen inside a sheet instead, without `Message`, and a
 * person looking at both saw two cards for one contact ("Except a person").
 *
 * The one person left in a layer is the one with no contact record, because
 * there is no contact screen to push for them.
 */
public sealed interface HeaderInfoPresentation {
    /** Push the contact screen — the same destination the contacts list opens. */
    public data class ContactScreen(val contactKey: String) : HeaderInfoPresentation

    /** The group's own layer over the conversation. */
    public data object GroupLayer : HeaderInfoPresentation

    /** The fallback layer for a person this device holds no contact record for. */
    public data object UnknownPersonLayer : HeaderInfoPresentation
}

/** Which presentation the header gives [info]. Total, so a caller cannot forget one. */
public fun headerInfoPresentation(info: ConversationInfo): HeaderInfoPresentation = when (info) {
    is ConversationInfo.Person -> HeaderInfoPresentation.ContactScreen(info.contactKey)
    ConversationInfo.Group -> HeaderInfoPresentation.GroupLayer
    ConversationInfo.PersonUnknown -> HeaderInfoPresentation.UnknownPersonLayer
}
