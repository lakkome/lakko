// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.CONTACT_ACCOUNT_ID_LEN

/*
 * `Message` on a contact's card (maintainer, 2026-11-05): a person in contacts
 * can always be written to. With a chat on this device it opens; with an
 * account id and no chat here it founds a new one-to-one with that contact and
 * opens it; only a contact with no account id cannot be messaged from here.
 */

/** What `Message` does for a contact. */
internal sealed interface MessageAction {
    /** The one-to-one is on this device: open it. */
    data class Open(val groupIdHex: String) : MessageAction

    /** No chat here, an account id known: found one, then open it. */
    data object Found : MessageAction

    /** No account id: nothing can be founded (`contacts.detail.message_unavailable`). */
    data object Unavailable : MessageAction
}

internal fun messageAction(chatGroupIdHex: String?, accountId: ByteArray?): MessageAction = when {
    chatGroupIdHex != null -> MessageAction.Open(chatGroupIdHex)
    accountId != null && accountId.size == CONTACT_ACCOUNT_ID_LEN -> MessageAction.Found
    else -> MessageAction.Unavailable
}

/** The line a failed or refused `Message` leaves under the button. */
public enum class MessageNotice {
    /** Founding the one-to-one failed: `contacts.error.start_chat`. */
    START_FAILED,

    /** The request inside a group could not be sent: `contacts.detail.request_failed`. */
    REQUEST_FAILED,

    /** No grant and no shared group: `contacts.detail.cannot_message`. */
    CANNOT_MESSAGE,
}

/**
 * Runs what `Message` does for a known person with no chat on this device and
 * keeps its progress per contact (route key): the button shows progress while
 * it runs and, after, the [MessageNotice] it ended in, staying usable.
 *
 * [found] is `ContactsRepository.messageKnownPerson`: through a grant that
 * still reaches the person it founds the one-to-one
 * ([me.lakko.app.contacts.foundOneToOneWith], linked onto the existing row);
 * else the contact request goes inside a shared group
 * (`docs/ux/who-can-find-me.md` section 6.4); else nothing is sent. A throw is
 * a failed founding.
 */
internal class ContactChatStarter(
    private val found: suspend (AcceptedContact) -> me.lakko.app.contacts.MessageOutcome,
    private val logFailure: (String) -> Unit = { android.util.Log.w("LakkoContacts", it) },
) {
    private val mutableStarting = MutableStateFlow<Set<String>>(emptySet())
    private val mutableNotices = MutableStateFlow<Map<String, MessageNotice>>(emptyMap())

    /** Contacts whose `Message` is running. */
    val starting: StateFlow<Set<String>> = mutableStarting.asStateFlow()

    /** The line each contact's last `Message` left, if any. */
    val notices: StateFlow<Map<String, MessageNotice>> = mutableNotices.asStateFlow()

    /**
     * Run `Message` for [contact]. Returns the one-to-one to open, or `null`
     * (a request was sent, nothing could be sent, it failed, or a run for this
     * contact is already going: a second tap does nothing).
     */
    suspend fun start(routeKey: String, contact: AcceptedContact): String? {
        var claimed = false
        mutableStarting.update { running ->
            claimed = routeKey !in running
            if (claimed) running + routeKey else running
        }
        if (!claimed) return null
        mutableNotices.update { it - routeKey }
        return try {
            when (val outcome = found(contact)) {
                is me.lakko.app.contacts.MessageOutcome.Opened -> outcome.groupIdHex
                me.lakko.app.contacts.MessageOutcome.RequestSent -> null
                me.lakko.app.contacts.MessageOutcome.CannotMessage -> {
                    mutableNotices.update { it + (routeKey to MessageNotice.CANNOT_MESSAGE) }
                    null
                }
                me.lakko.app.contacts.MessageOutcome.RequestFailed -> {
                    mutableNotices.update { it + (routeKey to MessageNotice.REQUEST_FAILED) }
                    null
                }
            }
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            logFailure("WARN message: founding the one-to-one failed: ${cause.javaClass.simpleName}")
            mutableNotices.update { it + (routeKey to MessageNotice.START_FAILED) }
            null
        } finally {
            mutableStarting.update { it - routeKey }
        }
    }
}
