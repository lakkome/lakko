// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import android.util.Log

/*
 * `docs/ux/message-actions.md` section 9 — "A notification already on screen
 * for the deleted message is withdrawn where the platform lets the app do
 * that." Android lets an app read its own posted notifications
 * (`NotificationManager.getActiveNotifications`), so a conversation
 * notification carries, in its extras, the DS message it describes: its group,
 * its id and its author. When a deletion of that message is applied, here in
 * the background or in the running app, the notification is cancelled. A
 * notification without the extras (the generic "New message", one posted by
 * an older build) is left alone.
 */

/** Notification extra: the DS group (hex) of the message a notification shows. */
public const val EXTRA_PREVIEW_GROUP_HEX: String = "lakko.previewGroupHex"

/** Notification extra: the DS id of the message a notification shows (Long). */
public const val EXTRA_PREVIEW_MESSAGE_ID: String = "lakko.previewMessageId"

/** Notification extra: the account key (base64) of that message's author. */
public const val EXTRA_PREVIEW_AUTHOR_KEY: String = "lakko.previewAuthorKey"

/** One posted notification as the withdrawal rule reads it. */
public data class ShownNotification(
    val tag: String?,
    val id: Int,
    /** [EXTRA_PREVIEW_GROUP_HEX], or `null` when absent. */
    val groupHex: String?,
    /** [EXTRA_PREVIEW_MESSAGE_ID], or `null` when absent. */
    val messageId: Long?,
    /** [EXTRA_PREVIEW_AUTHOR_KEY], or `null` when absent. */
    val authorKeyB64: String?,
)

/**
 * A deletion that was applied. [deleterKeyB64] is the account key of the op's
 * sender when the deletion was NOT checked against the message's author (the
 * background read holds no message log); `null` when it was (the running app
 * applies a delete only from the message's author, ADR-0023).
 */
public data class AppliedDelete(val messageId: Long, val deleterKeyB64: String? = null)

/**
 * The notifications to withdraw for [deletes] in the group [groupHex]: those
 * that describe one of the deleted messages of that group. A notification
 * without the extras is never withdrawn. An unchecked deletion withdraws only
 * a notification whose recorded author is the deleter, so a member cannot
 * make another person's message vanish from the shade. Pure.
 */
public fun notificationsToWithdraw(
    active: List<ShownNotification>,
    groupHex: String,
    deletes: List<AppliedDelete>,
): List<ShownNotification> {
    if (deletes.isEmpty()) return emptyList()
    val group = groupHex.lowercase()
    val byId = deletes.groupBy { it.messageId }
    return active.filter { n ->
        val id = n.messageId ?: return@filter false
        if (n.groupHex?.lowercase() != group) return@filter false
        val matching = byId[id] ?: return@filter false
        matching.any { d -> d.deleterKeyB64 == null || (n.authorKeyB64 != null && n.authorKeyB64 == d.deleterKeyB64) }
    }
}

/**
 * Cancel the posted notifications [notificationsToWithdraw] names. Best
 * effort: any failure leaves the notification where it is.
 */
public fun withdrawDeletedMessageNotifications(context: Context, groupHex: String, deletes: List<AppliedDelete>) {
    if (deletes.isEmpty()) return
    try {
        val manager = context.getSystemService(android.app.NotificationManager::class.java) ?: return
        val active = manager.activeNotifications.map { sbn ->
            val extras = sbn.notification?.extras
            ShownNotification(
                tag = sbn.tag,
                id = sbn.id,
                groupHex = extras?.getString(EXTRA_PREVIEW_GROUP_HEX),
                messageId = extras?.takeIf { it.containsKey(EXTRA_PREVIEW_MESSAGE_ID) }?.getLong(EXTRA_PREVIEW_MESSAGE_ID),
                authorKeyB64 = extras?.getString(EXTRA_PREVIEW_AUTHOR_KEY),
            )
        }
        for (n in notificationsToWithdraw(active, groupHex, deletes)) {
            manager.cancel(n.tag, n.id)
        }
    } catch (cause: Throwable) {
        Log.w("DeletedMessageNotes", "withdraw failed (${cause.javaClass.simpleName})")
    }
}
