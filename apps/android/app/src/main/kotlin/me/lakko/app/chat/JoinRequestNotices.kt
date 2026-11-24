// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * `docs/ux/group-and-channel-privacy.md` section 8.1 — telling the owner and
 * admins a request to join is waiting: a bar in the conversation, an unread
 * mark on the Chats row, and one notification per request.
 *
 * Every rule here is a pure function of its arguments, so the host suite runs
 * them without the native core. The repository feeds them live group state
 * and keeps the per-device marks ([JoinRequestMarks]) in `GroupSettingsStore`.
 * Requests are named by the joiner's signature key in standard base64.
 */

/**
 * What this device remembers about the requests of one group, per account.
 * Local, never synced.
 */
public data class JoinRequestMarks(
    /** Requests already announced, by notification or by the bar. A request
     *  in here is never announced again on this device. */
    val announced: Set<String> = emptySet(),
    /** Requests this device has not yet shown on screen. While this is not
     *  empty the group's Chats row is unread. */
    val unshown: Set<String> = emptySet(),
) {
    val isEmpty: Boolean get() = announced.isEmpty() && unshown.isEmpty()
}

/**
 * The role gate of all three signals: the owner or an admin of the group
 * (the gate that shows `Approve` and `Reject`), whose membership has not
 * ended.
 */
public fun joinRequestSignalsApply(ownerOrAdmin: Boolean, membershipEnded: Boolean): Boolean =
    ownerOrAdmin && !membershipEnded

/** The catalog key of the bar's text, and the count it takes (none for the full form). */
public data class JoinRequestBannerText(val key: String, val count: Int?)

/** The heading of the `Join requests` section reads `64+` from this many requests. */
public const val JOIN_REQUESTS_FULL_COUNT: Int = 64

public const val JOIN_REQUESTS_BANNER_KEY: String = "chat.join_requests.banner"
public const val JOIN_REQUESTS_BANNER_FULL_KEY: String = "chat.join_requests.banner_full"

/**
 * The bar at the top of the conversation, or `null` when it is not drawn:
 * not for this person, or no request waiting. [queueFull] and a count of
 * [JOIN_REQUESTS_FULL_COUNT] or more give the `64+` form, the same rule the
 * members panel heading uses.
 */
public fun joinRequestBanner(signalsApply: Boolean, pendingCount: Int, queueFull: Boolean): JoinRequestBannerText? {
    if (!signalsApply || pendingCount <= 0) return null
    return if (queueFull || pendingCount >= JOIN_REQUESTS_FULL_COUNT) {
        JoinRequestBannerText(JOIN_REQUESTS_BANNER_FULL_KEY, null)
    } else {
        JoinRequestBannerText(JOIN_REQUESTS_BANNER_KEY, pendingCount)
    }
}

/** The marks after a request was read, and whether to post its notification. */
public data class JoinRequestArrival(val marks: JoinRequestMarks, val notify: Boolean)

/**
 * A request [joinerKey] was read (newly kept, or asked again while already
 * waiting).
 *
 * - Already announced: nothing changes.
 * - Else it is announced now. With the conversation open in front of the
 *   person ([openAndVisible]) the bar is the signal and the request counts as
 *   shown. Otherwise it is not shown yet, and a notification is posted unless
 *   [notificationAllowed] is false (the group is muted).
 */
public fun onJoinRequestArrived(
    marks: JoinRequestMarks,
    joinerKey: String,
    openAndVisible: Boolean,
    notificationAllowed: Boolean,
): JoinRequestArrival {
    if (joinerKey in marks.announced) return JoinRequestArrival(marks, notify = false)
    val announced = marks.announced + joinerKey
    if (openAndVisible) return JoinRequestArrival(marks.copy(announced = announced), notify = false)
    return JoinRequestArrival(
        JoinRequestMarks(announced = announced, unshown = marks.unshown + joinerKey),
        notify = notificationAllowed,
    )
}

/** The bar was drawn in the open conversation: every request is shown. */
public fun markJoinRequestsShown(marks: JoinRequestMarks): JoinRequestMarks =
    if (marks.unshown.isEmpty()) marks else marks.copy(unshown = emptySet())

/** Both sets keep only the requests still waiting ([pending]). */
public fun pruneJoinRequestMarks(marks: JoinRequestMarks, pending: Set<String>): JoinRequestMarks {
    val announced = marks.announced.filterTo(LinkedHashSet()) { it in pending }
    val unshown = marks.unshown.filterTo(LinkedHashSet()) { it in pending }
    if (announced.size == marks.announced.size && unshown.size == marks.unshown.size) return marks
    return JoinRequestMarks(announced, unshown)
}

/**
 * The Chats row's unread count: the unread messages plus one per request not
 * yet shown. Never negative, never overflowing.
 */
public fun joinRequestRowUnread(messageUnread: Int, unshownCount: Int): Int {
    val sum = messageUnread.toLong().coerceAtLeast(0) + unshownCount.toLong().coerceAtLeast(0)
    return sum.coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
}

public const val JOIN_REQUEST_NOTICE_TITLE_KEY: String = "notification.join_request.title"
public const val JOIN_REQUEST_NOTICE_BODY_KEY: String = "notification.join_request.body"
public const val JOIN_REQUEST_NOTICE_BODY_UNNAMED_KEY: String = "notification.join_request.body_unnamed"

/** The catalog key of the notification's body and its arguments. */
public data class JoinRequestNoticeText(val bodyKey: String, val texts: Map<String, String>)

/**
 * The notification's body: `{name} asks to join {group}` with the name the
 * caller resolved (the contact name, else the name the request carries), or
 * `Someone asks to join {group}` with no name at all.
 */
public fun joinRequestNotice(name: String?, groupTitle: String): JoinRequestNoticeText {
    val named = name?.trim()?.takeIf { it.isNotEmpty() }
    return if (named != null) {
        JoinRequestNoticeText(JOIN_REQUEST_NOTICE_BODY_KEY, mapOf("name" to named, "group" to groupTitle))
    } else {
        JoinRequestNoticeText(JOIN_REQUEST_NOTICE_BODY_UNNAMED_KEY, mapOf("group" to groupTitle))
    }
}

/** The notification tag of one request, so each request has its own. */
public fun joinRequestNotificationTag(groupIdHex: String, joinerKey: String): String =
    "join-request:" + groupIdHex.lowercase() + ":" + joinerKey

/** The stored form of one set: the keys, sorted, comma-separated (base64 has no comma). */
public fun encodeJoinRequestKeys(keys: Set<String>): String? =
    keys.filter { it.isNotEmpty() && ',' !in it }.sorted().joinToString(",").ifEmpty { null }

public fun decodeJoinRequestKeys(stored: String?): Set<String> =
    stored?.split(',')?.filterTo(LinkedHashSet()) { it.isNotEmpty() } ?: emptySet()
