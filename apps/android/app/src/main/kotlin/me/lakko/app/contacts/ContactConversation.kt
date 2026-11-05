// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

/*
 * Which conversation on this device is THE one-to-one with a contact, and how
 * one is founded when there is none. Android twin of
 * `apps/web/src/contacts/contact-conversation.ts` and
 * `apps/web/src/contacts/found-one-to-one.ts`.
 *
 * Two screens need it and must agree: Contacts (`Message` on a contact's
 * card) and the call screen's `Invite contacts`
 * (`docs/ux/call-links.md` section 3), which sends the invitation into that
 * conversation and founds it first when the person has never written to the
 * contact from this device.
 *
 * ## The rule
 *
 * A person's chat is a conversation whose kind is one-to-one (ADR-0062) and
 * whose PEOPLE are exactly this account and the contact. People, not leaves: a
 * person is every one of their devices (ADR-0050), so a roster of three leaves
 * can be two people, and the contact is known by the account key (or a key of
 * the same person across a proven rotation, ADR-0074 section 3.7).
 *
 * 1. The conversation linked on the contact record, when the registry still
 *    holds it AND it is such a one-to-one. A linked id whose chat is gone
 *    (seen live on web 2026-09-26 as "Conversation not found"), that is a
 *    group, or that is a one-to-one with someone else is not an address
 *    (seen live on web 2026-11-05: Message opened a group the person is in).
 * 2. Else the first held conversation that is such a one-to-one.
 *
 * A two-person GROUP is never the answer: it is a group that happens to hold
 * two people, and writing an invitation there is not writing to the contact.
 *
 * Pure: the readers are passed in, so a host test runs it without a native
 * group handle.
 */

/**
 * The one-to-one with the contact holding [contactKey], among
 * [conversations], or `null`.
 *
 * @param contactKey the contact's person (account) key, in the same encoding
 *   [peopleOf] and [ownKey] use.
 * @param linkedId the conversation id linked on the contact record, or `null`.
 * @param ownKey this account's person key, or `null` before a session.
 * @param peopleOf the distinct people of a conversation; a torn handle reads
 *   as empty.
 * @param isOneToOne whether the conversation's kind is one-to-one.
 * @param samePerson whether two person keys are one person (proven
 *   rotations); defaults to equality.
 */
internal fun <G> conversationWithContact(
    contactKey: String,
    linkedId: String?,
    conversations: Collection<G>,
    idOf: (G) -> String,
    ownKey: String?,
    peopleOf: (G) -> List<String>,
    isOneToOne: (G) -> Boolean,
    samePerson: (String, String) -> Boolean = { a, b -> a == b },
): G? {
    if (ownKey == null) return null
    fun isTheirs(conversation: G): Boolean {
        if (!isOneToOne(conversation)) return false
        val people = peopleOf(conversation)
        if (people.size != 2 || ownKey !in people) return false
        val other = people.firstOrNull { it != ownKey } ?: return false
        return samePerson(other, contactKey)
    }
    if (linkedId != null) {
        conversations.firstOrNull { idOf(it) == linkedId }?.takeIf(::isTheirs)?.let { return it }
    }
    return conversations.firstOrNull(::isTheirs)
}

/**
 * A one-to-one cannot be founded with this contact: its record carries no
 * account id (a contact stored from a v1 request, before ADR-0021), and the
 * directory serves a person's devices by account id.
 */
public class ContactHasNoAccountId : IllegalStateException("the contact record has no account id")

/**
 * Found the one-to-one conversation with [contact] and link it back to the
 * contact record. Returns the new conversation's id.
 *
 * [startOneToOne] is the one creation path
 * (`ChatRepository.startGroupWith` with the one-to-one kind, the function a
 * contact accept runs): fetch the person's device KeyPackages, create the
 * group, deliver the Welcome. [link] records the id on the contact
 * (`ContactsRepository.linkOneToOne`). Nothing is created for a contact
 * with no account id.
 */
internal suspend fun foundOneToOneWith(
    contact: AcceptedContact,
    startOneToOne: suspend (accountId: ByteArray, signatureKey: ByteArray) -> String,
    link: (signatureKey: ByteArray, groupIdHex: String) -> Unit,
): String {
    val accountId = contact.request.fromAccountId ?: throw ContactHasNoAccountId()
    val groupIdHex = startOneToOne(accountId, contact.request.fromPubkey)
    link(contact.request.fromPubkey, groupIdHex)
    return groupIdHex
}
