// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

/*
 * UX `reporting-and-moderation.md` section 2.1 — one subject per entry point,
 * built here so every entry point reports the same target with the same
 * evidence and the same `Also` checkbox.
 */

/** A person, from their card or a contact request: their own messages can be picked. */
public fun personReportSubject(
    accountKeyB64: String,
    name: String,
    conversationKind: ReportConversationKind,
    groupIdHex: String?,
    initialMessages: List<ReportableMessage> = emptyList(),
    other: List<ReportOtherEvidence> = emptyList(),
): ReportSubject = ReportSubject(
    target = ReportTargetRef.Account(accountKeyB64),
    name = name,
    conversationKind = conversationKind,
    pickGroupIdHex = groupIdHex,
    pickAuthorKeyB64 = accountKeyB64,
    initialMessages = initialMessages,
    other = other,
    also = ReportAlso.Block(accountKeyB64),
)

/** A message's author, from the message menu: that message is included. */
public fun messageReportSubject(
    authorKeyB64: String,
    authorName: String,
    groupIdHex: String,
    oneToOne: Boolean,
    message: ReportableMessage,
): ReportSubject = personReportSubject(
    accountKeyB64 = authorKeyB64,
    name = authorName,
    conversationKind = if (oneToOne) ReportConversationKind.ONE_TO_ONE else ReportConversationKind.GROUP,
    groupIdHex = groupIdHex,
    initialMessages = listOf(message),
)

/** A contact request, from the Requests row: the request itself is included. */
public fun contactRequestReportSubject(
    requesterKeyB64: String,
    name: String,
    requestWire: ByteArray?,
): ReportSubject = personReportSubject(
    accountKeyB64 = requesterKeyB64,
    name = name,
    conversationKind = ReportConversationKind.CONTACT_REQUEST,
    groupIdHex = null,
    other = listOfNotNull(requestWire?.let { ReportOtherEvidence.ContactRequest(it) }),
)

/** A group, from its info: reported only by its `@name`; any other member's messages can be picked. */
public fun groupReportSubject(handle: String, name: String, groupIdHex: String): ReportSubject = ReportSubject(
    target = ReportTargetRef.GroupHandle(handle),
    name = name,
    conversationKind = ReportConversationKind.GROUP,
    pickGroupIdHex = groupIdHex,
    pickAuthorKeyB64 = null,
    initialMessages = emptyList(),
    other = emptyList(),
    also = ReportAlso.Leave(groupIdHex),
)

/** A channel, from its info, a post menu or a Discover card. */
public fun channelReportSubject(
    channelIdHex: String,
    name: String,
    conversationKind: ReportConversationKind,
    other: List<ReportOtherEvidence> = emptyList(),
    subscribed: Boolean = true,
    /** Channel info: `Add messages` picks from the channel's stored posts. */
    pickPosts: Boolean = false,
): ReportSubject = ReportSubject(
    target = ReportTargetRef.Channel(channelIdHex),
    name = name,
    conversationKind = conversationKind,
    pickGroupIdHex = null,
    pickAuthorKeyB64 = null,
    initialMessages = emptyList(),
    pickChannelIdHex = channelIdHex.takeIf { pickPosts },
    other = other,
    also = if (subscribed) ReportAlso.Unsubscribe(channelIdHex) else ReportAlso.None,
)
