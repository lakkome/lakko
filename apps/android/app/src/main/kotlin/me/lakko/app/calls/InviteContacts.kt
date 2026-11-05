// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.MessageOutcome
import me.lakko.app.ui.contacts.ContactRowModel
import me.lakko.app.ui.contacts.contactSections
import me.lakko.app.ui.contacts.filterContacts

/*
 * The list `Invite contacts` shows on the call screen, and what choosing a
 * row does (`docs/ux/call-links.md` section 3). Android twin of
 * `apps/web/src/calls/invite-contacts.ts` and of `sendLinkCallInvite` in
 * `apps/web/src/calls/CallHostContext.tsx`.
 *
 * The person's CONTACTS, not their conversations: an invitation is addressed
 * to a person, and a contact the person has never written to is as invitable
 * as one they talk to daily. (The first version listed conversations, so a
 * person with contacts and no chats saw no way to invite anyone, and groups
 * were offered as invitees; corrected 2026-10-02.) Groups are not in it.
 *
 * Pure: resolved rows come in, and the send runs over ports.
 */

/**
 * The rows, one per contact, in the order the Contacts list shows people:
 * by name, an unnamed row by its username, rows that start with no letter
 * last. The rule is the Contacts list's own ([contactSections]).
 */
public fun inviteContactRows(contacts: List<ContactRowModel>): List<ContactRowModel> =
    contactSections(contacts.distinctBy { it.key }).flatMap { it.rows }

/** The rows a typed query keeps: by name or by username (the Contacts
 *  list's own filter). */
public fun filterInviteContactRows(rows: List<ContactRowModel>, query: String): List<ContactRowModel> =
    filterContacts(rows, query)

/**
 * The sentence the list shows in place of rows, or `null` when it has rows
 * to show: no contacts at all, or contacts none of which match the search.
 */
public fun inviteContactsEmptyKey(contactCount: Int, visibleCount: Int): String? = when {
    contactCount == 0 -> "call.invite.picker_empty"
    visibleCount == 0 -> "chats.search.no_results"
    else -> null
}

/** What [inviteContactToCall] reads and drives. */
internal interface InviteContactPorts {
    /** The contact record a row stands for, or `null` when it is gone. */
    fun contactFor(rowKey: String): AcceptedContact?

    /** The one-to-one with the contact on this device, or `null`
     *  (`me.lakko.app.contacts.conversationWithContact`). */
    fun conversationWith(contact: AcceptedContact): String?

    /**
     * The one order for reaching a known person with no chat on this device
     * (`docs/ux/who-can-find-me.md` 6.4, `ContactsRepository.messageKnownPerson`):
     * a grant that still reaches them founds the one-to-one
     * (`me.lakko.app.contacts.foundOneToOneWith`, linked on the contact
     * record); else a contact request goes inside a group both are in; else
     * nothing. Throws when a founding fails.
     */
    suspend fun messageKnownPerson(contact: AcceptedContact): MessageOutcome

    /** The conversation's ordinary send: one plain text message, recorded in
     *  the conversation like any other sent message. */
    suspend fun sendText(groupIdHex: String, text: String)
}

/** What choosing a contact in `Invite contacts` came to. */
internal enum class ContactInviteOutcome {
    /** The invitation is in the one-to-one with them. */
    SENT,

    /** No one-to-one, no working grant: a contact request went inside a group
     *  both are in (or one is already waiting). No invitation until they
     *  accept. */
    REQUEST_SENT,

    /** No one-to-one, no grant, no shared group: nothing was sent. */
    CANNOT_MESSAGE,

    /** Not sent (the contact is gone, a founding or a send failed). */
    FAILED,
}

/**
 * Send the call's invitation to the contact a row stands for: into the
 * one-to-one with that contact. When this device holds none, the one order
 * of `docs/ux/who-can-find-me.md` 6.4 runs first: a working grant founds it,
 * else a contact request goes inside a shared group and no invitation is sent
 * (nothing is minted), else nothing is sent.
 *
 * [sendInvite] is [LinkCallHost.sendInvite] bound to the session and the
 * contact's name: it mints the call's link on first use, runs the block it
 * is given with that link, and records the outcome for the call screen's
 * status line. A failed send keeps the link, so the next attempt mints
 * nothing. The call is never affected.
 *
 * `true` when the invitation was sent. `false` when it was not: the contact
 * is gone (nothing is minted then), its record has no account id to found a
 * conversation with (nothing is created), or the send failed.
 */
internal suspend fun inviteContactToCall(
    ports: InviteContactPorts,
    rowKey: String,
    sendInvite: suspend (send: suspend (url: String) -> Unit) -> Boolean,
): ContactInviteOutcome {
    val contact = ports.contactFor(rowKey) ?: return ContactInviteOutcome.FAILED
    val groupIdHex = ports.conversationWith(contact) ?: run {
        val outcome = try {
            ports.messageKnownPerson(contact)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            return ContactInviteOutcome.FAILED
        }
        when (outcome) {
            is MessageOutcome.Opened -> outcome.groupIdHex
            MessageOutcome.RequestSent -> return ContactInviteOutcome.REQUEST_SENT
            MessageOutcome.CannotMessage -> return ContactInviteOutcome.CANNOT_MESSAGE
            MessageOutcome.RequestFailed -> return ContactInviteOutcome.FAILED
        }
    }
    return if (sendInvite { url -> ports.sendText(groupIdHex, url) }) {
        ContactInviteOutcome.SENT
    } else {
        ContactInviteOutcome.FAILED
    }
}
