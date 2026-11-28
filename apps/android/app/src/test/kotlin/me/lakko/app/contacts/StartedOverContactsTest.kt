// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.chat.DeleteChatPlan
import me.lakko.app.chat.EndSource
import me.lakko.app.chat.OwedEnd
import me.lakko.app.chat.chosenEnd
import me.lakko.app.chat.deleteChatPlan
import me.lakko.app.ui.auth.RecoverMode
import me.lakko.app.ui.auth.recoverListGroups
import me.lakko.app.ui.auth.recoverRowCopy
import me.lakko.app.ui.contacts.ContactRowModel
import me.lakko.app.ui.contacts.ContactRowSecondLine
import me.lakko.app.ui.contacts.contactRowSecondLine
import me.lakko.app.ui.contacts.personCardMembershipEnded
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Walk of 2026-11-28 (UX identity-recovery 16.11, ADR-0101 section 11): a
 * contact did an UNSEALED reset (same account id, new keys). The old row and
 * its closed one-to-one stay; the new account is a new contact; a sealed
 * reset leaves exactly one row; the old row is told apart; the recovery list
 * puts the email code with the sign-in ways.
 */
class StartedOverContactsTest {

    private val account = "QUNDT1VOVC1BTllBLTAxMg=="
    private val oldKey = "b2xkLWtleQ=="
    private val newKey = "bmV3LWtleQ=="

    // ---- a1: the invite page of the new account ----------------------------

    /** The page decision as [ContactsRepository.classifyOpenedInvite] makes
     *  it: the established row by account, then the key comparison. */
    private fun page(
        rows: List<ContactRowKeys>,
        pinKeys: List<String>,
        inviteKey: String,
        startedOverKeys: Set<String>,
        startedOverNewKey: String?,
    ): InvitePageKind {
        val index = establishedContactOfAccount(rows, account, pinKeys, bindingKeyB64 = null)
        val established = index?.let { rows[it] }
            ?.takeIf { establishedRowCountsForKey(it.keyB64, inviteKey, startedOverKeys, startedOverNewKey) }
        return invitePageKind(isOwn = false, established = established != null, pendingInviter = false)
    }

    @Test
    fun `the new account's link is the ordinary Add page, not already in your contacts`() {
        val rows = listOf(ContactRowKeys(account, oldKey))
        // The unsealed reset is recorded: the old key started over and the
        // record names the new key. The pin still names the old key.
        assertEquals(
            InvitePageKind.DEFAULT,
            page(rows, listOf(oldKey), newKey, setOf(oldKey), startedOverNewKey = newKey),
        )
        // After the person accepted the new account elsewhere (the record is
        // closed, the old key stays a started-over key): still not the old row.
        assertEquals(
            InvitePageKind.DEFAULT,
            page(rows, listOf(oldKey), newKey, setOf(oldKey), startedOverNewKey = null),
        )
    }

    @Test
    fun `the new account is already a contact once its own row exists`() {
        val rows = listOf(ContactRowKeys(account, oldKey), ContactRowKeys(account, newKey))
        // The new key is the pin now; its row is the established one.
        assertEquals(InvitePageKind.ALREADY, page(rows, listOf(newKey), newKey, setOf(oldKey), null))
    }

    @Test
    fun `a contact's own link, a rotation and an unnoticed change keep the already page`() {
        assertTrue(establishedRowCountsForKey(oldKey, oldKey, setOf(oldKey), newKey))
        assertTrue(establishedRowCountsForKey(oldKey, newKey, emptySet(), null))
        assertFalse(establishedRowCountsForKey(oldKey, newKey, setOf(oldKey), null))
        assertFalse(establishedRowCountsForKey(oldKey, newKey, emptySet(), newKey))
        assertTrue(establishedRowCountsForKey(oldKey, "other", emptySet(), newKey))
    }

    // ---- a2: a request from the new keys ------------------------------------

    @Test
    fun `a request from the new keys is listed with the note and never answered silently`() {
        val verdict = knownContactRequestVerdict(
            senderIsContact = true,
            check = KnownRequestCheck.STARTED_OVER,
            requestKeyB64 = newKey,
            trustedKeyB64 = oldKey,
            holdsOneToOne = true,
            startedOverKeyB64 = newKey,
        )
        // Not ANSWER: the silent answer never applies to the new key. With a
        // link answer the request then goes on to the link decision as any
        // stranger's (no card); without one it is the card with the note.
        assertEquals(KnownRequestVerdict.SHOW_STARTED_OVER, verdict)
        assertEquals(
            KnownRequestVerdict.DROP,
            knownContactRequestVerdict(
                senderIsContact = true,
                check = KnownRequestCheck.STARTED_OVER,
                requestKeyB64 = "someone-else",
                trustedKeyB64 = oldKey,
                startedOverKeyB64 = newKey,
            ),
        )
    }

    @Test
    fun `accepting the new account adds a new row and leaves the old one as it was`() {
        val old = contact(oldKey, group = "old-chat")
        val rows = acceptingRequests(listOf(old), listOf(request(newKey)), groupIdHex = "new-chat")
        assertEquals(2, rows.size)
        assertEquals(old, rows[0])
        assertEquals("new-chat", rows[1].groupIdHex)
        assertTrue(rows[1].request.fromPubkey.contentEquals(bytes(newKey)))
    }

    // ---- b: one row after a sealed reset -------------------------------------

    @Test
    fun `a key a sealed reset retired never gets a row of its own`() {
        assertFalse(oneToOnePeerGetsRow(oldKey, supersededKeysB64 = setOf(oldKey)))
        assertTrue(oneToOnePeerGetsRow(newKey, supersededKeysB64 = setOf(oldKey)))
        // An unsealed reset retires nothing: the old row stays its own.
        assertTrue(oneToOnePeerGetsRow(oldKey, supersededKeysB64 = emptySet()))
    }

    @Test
    fun `a sealed reset moves the one row and folds a second under the new key`() {
        val rows = listOf(ContactRowKeys(account, oldKey), ContactRowKeys(account, newKey))
        val plan = rekeyContactRows(rows, account, retiredKeysB64 = listOf(oldKey), newKeyB64 = newKey)!!
        assertEquals(0, plan.rowIndex)
        assertEquals(setOf(1), plan.foldedIndices)
    }

    @Test
    fun `a sealed reset never moves the row of an account that started over`() {
        val older = "b2xkZXI="
        // The person started over earlier (older key), then did a sealed reset
        // of the new account (oldKey -> newKey).
        val rows = listOf(ContactRowKeys(account, older), ContactRowKeys(account, oldKey))
        val plan = rekeyContactRows(
            rows,
            account,
            retiredKeysB64 = listOf(oldKey),
            newKeyB64 = newKey,
            startedOverKeysB64 = setOf(older),
        )!!
        assertEquals(1, plan.rowIndex)
        assertTrue(plan.foldedIndices.isEmpty())
    }

    @Test
    fun `the old started-over row reads so under the name instead of the username`() {
        val old = ContactRowModel(key = "k1", name = "Anya Seal", username = "w28a0950", startedOver = true)
        val fresh = old.copy(key = "k2", startedOver = false)
        assertEquals(ContactRowSecondLine.StartedOver, contactRowSecondLine(old))
        assertEquals(ContactRowSecondLine.Username("w28a0950"), contactRowSecondLine(fresh))
        assertNull(contactRowSecondLine(fresh.copy(name = "@w28a0950")))
        assertNull(contactRowSecondLine(fresh.copy(username = null)))
    }

    // ---- c: the old card ------------------------------------------------------

    @Test
    fun `the started-over card is the card of a closed one-to-one`() {
        assertTrue(personCardMembershipEnded(membershipEnded = false, keyStartedOver = true))
        assertTrue(personCardMembershipEnded(membershipEnded = true, keyStartedOver = false))
        assertFalse(personCardMembershipEnded(membershipEnded = false, keyStartedOver = false))
    }

    @Test
    fun `delete chat deletes the closed one-to-one of an account that started over`() {
        // Membership ended here: deleted, MLS state with it.
        assertEquals(DeleteChatPlan.DELETE, deleteChatPlan(active = false, peerStartedOver = false))
        // No readable group left: nothing of it to keep.
        assertEquals(DeleteChatPlan.DELETE, deleteChatPlan(active = null, peerStartedOver = false))
        // The old account's one-to-one still reads active: deleted, the MLS
        // state left inert because the core refuses an active group.
        assertEquals(
            DeleteChatPlan.DELETE_LEAVING_MLS_INERT,
            deleteChatPlan(active = true, peerStartedOver = true),
        )
        // A live conversation is never deleted by this button.
        assertEquals(DeleteChatPlan.REFUSED, deleteChatPlan(active = true, peerStartedOver = false))
    }

    @Test
    fun `the owed delete row carries the inert flag and older rows read without it`() {
        val row = chosenEnd("AB12", 0u, nowMs = 5, leaveMlsInert = true)
        assertEquals(EndSource.CHOSEN, row.source)
        assertEquals("ab12", row.idHex)
        assertTrue(row.leaveMlsInert)
        assertFalse(chosenEnd("ab12", 0u, nowMs = 5).leaveMlsInert)
        val json = kotlinx.serialization.json.Json { ignoreUnknownKeys = true }
        val older = json.decodeFromString(
            OwedEnd.serializer(),
            "{\"idHex\":\"ab12\",\"name\":\"\",\"left\":true,\"epoch\":\"0\",\"source\":\"CHOSEN\",\"stage\":\"STOPPED\"}",
        )
        assertFalse(older.leaveMlsInert)
        assertEquals(row, json.decodeFromString(OwedEnd.serializer(), json.encodeToString(OwedEnd.serializer(), row)))
    }

    // ---- d: the recovery list -------------------------------------------------

    @Test
    fun `the email code is a sign-in way, after the authenticator code`() {
        val groups = recoverListGroups(threeWords = true)
        assertEquals(listOf("recovery.group.keys", "recovery.group.signin"), groups.map { it.labelKey })
        assertEquals(listOf(RecoverMode.AnotherDevice, RecoverMode.Passkey, RecoverMode.Phrase), groups[0].modes)
        assertEquals(listOf(RecoverMode.Words, RecoverMode.Authenticator, RecoverMode.Email), groups[1].modes)
        assertEquals(
            listOf(RecoverMode.Authenticator, RecoverMode.Email),
            recoverListGroups(threeWords = false)[1].modes,
        )
        assertEquals("recovery.email.row.title", recoverRowCopy(RecoverMode.Email).titleKey)
        // Every way is listed exactly once.
        assertEquals(RecoverMode.entries.toSet(), groups.flatMap { it.modes }.toSet())
        assertEquals(RecoverMode.entries.size, groups.sumOf { it.modes.size })
    }

    // ---- helpers ------------------------------------------------------------

    private fun bytes(b64: String): ByteArray = java.util.Base64.getDecoder().decode(b64)

    private fun request(keyB64: String): IncomingContactRequest = IncomingContactRequest(
        messageId = keyB64.hashCode().toLong().let { if (it < 0) -it else it } + 1,
        fromPubkey = bytes(keyB64),
        fromAccountId = bytes(account),
        fromPseudonym = ByteArray(32),
        validAfterMs = 0u,
        intro = null,
        createdAt = "2026-11-28T10:00:00Z",
    )

    private fun contact(keyB64: String, group: String?): AcceptedContact =
        AcceptedContact(request = request(keyB64), groupIdHex = group)
}
