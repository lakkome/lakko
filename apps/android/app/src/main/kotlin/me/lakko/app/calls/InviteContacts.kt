// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.contacts.AcceptedContact
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

    /** Found the one-to-one and link it on the contact record
     *  (`me.lakko.app.contacts.foundOneToOneWith`). Throws when it cannot. */
    suspend fun foundOneToOne(contact: AcceptedContact): String

    /** The conversation's ordinary send: one plain text message, recorded in
     *  the conversation like any other sent message. */
    suspend fun sendText(groupIdHex: String, text: String)
}

/**
 * Send the call's invitation to the contact a row stands for: into the
 * one-to-one with that contact, founded first when this device holds none.
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
): Boolean {
    val contact = ports.contactFor(rowKey) ?: return false
    return sendInvite { url ->
        val groupIdHex = ports.conversationWith(contact) ?: ports.foundOneToOne(contact)
        ports.sendText(groupIdHex, url)
    }
}
