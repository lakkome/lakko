// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.auth.SealedBlobStorage
import me.lakko.app.auth.base64Encode
import me.lakko.app.ui.contacts.InviteChatTarget
import me.lakko.app.ui.contacts.InviteLandingState
import me.lakko.app.ui.contacts.InviteLandingViewModel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.LinkRecord
import uniffi.lakko_ffi_uniffi.LinkRecordKind

/**
 * ADR-0079 sections 4 and 5 — the rules of a person added through their
 * invite link before their app answered, the order their messages are kept
 * in, the sealed store, the list rule over the synced link-record register,
 * and the invite page's state for each result of opening a link.
 */
class PendingContactsTest {

    private class MapBlobs : SealedBlobStorage {
        val map = mutableMapOf<String, ByteArray>()
        override fun read(name: String): ByteArray? = map[name]?.copyOf()
        override fun write(name: String, plaintext: ByteArray) {
            map[name] = plaintext.copyOf()
        }
        override fun delete(name: String) {
            map.remove(name)
        }
    }

    private fun contact(hex: String, key: Byte, at: Long = 1_000L, name: String = "Anna", username: String = "anna") =
        PendingContact(
            accountIdHex = hex,
            signaturePubkeyB64 = base64Encode(ByteArray(32) { key }),
            x25519PubkeyB64 = base64Encode(ByteArray(32) { (key + 1).toByte() }),
            displayName = name,
            username = username,
            addedAtMs = at,
            linkIdHex = "07".repeat(16),
        )

    /** ADR-0079 amendment of 2026-10-19 — the refusal is handed to the
     *  verifier with the pending contact's stated key, the link its request
     *  answered and this account's key; only a verifying refusal cancels
     *  (the signature itself is the core's, tested in `lakko-contacts`). */
    @Test
    fun `only a refusal verifying against the contact's key and link cancels it`() {
        val anna = contact("aa", 1)
        val bob = ByteArray(32) { 0xB0.toByte() }
        val refusal = byteArrayOf(9)
        var seen: List<ByteArray>? = null
        val annaSigned: (ByteArray, ByteArray, ByteArray, ByteArray) -> Boolean = { w, k, l, o ->
            seen = listOf(w, k, l, o)
            k.contentEquals(ByteArray(32) { 1 }) && l.contentEquals(ByteArray(16) { 7 }) && o.contentEquals(bob)
        }
        assertEquals("aa", PendingContacts.refusedBy(listOf(contact("bb", 3), anna), refusal, bob, annaSigned)?.accountIdHex)
        assertTrue(seen!![0].contentEquals(refusal))
        assertNull(PendingContacts.refusedBy(listOf(anna), refusal, ByteArray(32) { 0xB1.toByte() }, annaSigned))
        assertNull(PendingContacts.refusedBy(listOf(anna.copy(linkIdHex = "08".repeat(16))), refusal, bob, annaSigned))
        assertNull(PendingContacts.refusedBy(listOf(anna.copy(linkIdHex = "zz")), refusal, bob) { _, _, _, _ -> true })
    }

    /** Add racing a link that went dead: the consume answers dead and
     *  nothing is kept, no pending contact and no request. */
    @Test
    fun `Add on a link the consume finds dead keeps nothing`() = kotlinx.coroutines.runBlocking {
        val log = mutableListOf<String>()
        val outcome = InviteAddOrder.run(
            consumeLive = { log += "consume"; false },
            recordPending = { log += "record" },
            sendRequest = { log += "send" },
            forgetPending = { log += "forget" },
        )
        assertEquals(InviteAddOrder.Outcome.DEAD, outcome)
        assertEquals(listOf("consume"), log)
    }

    @Test
    fun `Add on a live link records before it sends and forgets when the send fails`() = kotlinx.coroutines.runBlocking {
        val log = mutableListOf<String>()
        assertEquals(
            InviteAddOrder.Outcome.ADDED,
            InviteAddOrder.run({ log += "consume"; true }, { log += "record" }, { log += "send" }, { log += "forget" }),
        )
        assertEquals(listOf("consume", "record", "send"), log)
        log.clear()
        val thrown = runCatching {
            InviteAddOrder.run(
                { log += "consume"; true },
                { log += "record" },
                { log += "send"; error("offline") },
                { log += "forget" },
            )
        }.exceptionOrNull()
        assertEquals("offline", thrown?.message)
        assertEquals(listOf("consume", "record", "send", "forget"), log)
    }

    /** Open chat on the page of a known contact (amended 2026-10-24). */
    @Test
    fun `Open chat as a known contact consumes, then sends, and keeps no pending contact`() =
        kotlinx.coroutines.runBlocking {
            val log = mutableListOf<String>()
            assertEquals(
                KnownContactAnswerOrder.Outcome.SENT,
                KnownContactAnswerOrder.run({ log += "consume"; true }, { log += "send" }),
            )
            assertEquals(listOf("consume", "send"), log)
        }

    @Test
    fun `Open chat as a known contact sends nothing on a dead link`() = kotlinx.coroutines.runBlocking {
        val log = mutableListOf<String>()
        assertEquals(
            KnownContactAnswerOrder.Outcome.DEAD,
            KnownContactAnswerOrder.run({ log += "consume"; false }, { log += "send" }),
        )
        assertEquals(listOf("consume"), log)
    }

    @Test
    fun `Open chat as a known contact lets a failed send reach the page`() = kotlinx.coroutines.runBlocking {
        val thrown = runCatching {
            KnownContactAnswerOrder.run({ true }, { error("offline") })
        }.exceptionOrNull()
        assertEquals("offline", thrown?.message)
    }

    /** UX section 3 (amended 2026-10-24): own > established > pending > default. */
    @Test
    fun `the invite page is the own link, then a contact, then a pending one, then Add`() {
        assertEquals(InvitePageKind.OWN, invitePageKind(isOwn = true, established = true, pendingInviter = true))
        assertEquals(InvitePageKind.ALREADY, invitePageKind(isOwn = false, established = true, pendingInviter = true))
        assertEquals(InvitePageKind.PENDING, invitePageKind(isOwn = false, established = false, pendingInviter = true))
        assertEquals(InvitePageKind.DEFAULT, invitePageKind(isOwn = false, established = false, pendingInviter = false))
    }

    @Test
    fun `a pending inviter's page opens the waiting chat and carries nothing to send`() {
        val state = InviteLandingViewModel.stateFor(InviteOpenResult.PendingInviter("ab", "Anna"))
        assertEquals(InviteLandingState.AlreadyContacts(InviteChatTarget.Pending("ab"), "Anna"), state)
        assertNull((state as InviteLandingState.AlreadyContacts).invite)
    }

    @Test
    fun `Add to a removed inviter sends before it restores and writes nothing pending`() = kotlinx.coroutines.runBlocking {
        val log = mutableListOf<String>()
        assertEquals(
            InviteRestoreOrder.Outcome.RESTORED,
            InviteRestoreOrder.run({ log += "consume"; true }, { log += "send" }, { log += "restore" }),
        )
        assertEquals(listOf("consume", "send", "restore"), log)
        log.clear()
        assertEquals(
            InviteRestoreOrder.Outcome.DEAD,
            InviteRestoreOrder.run({ log += "consume"; false }, { log += "send" }, { log += "restore" }),
        )
        assertEquals(listOf("consume"), log)
        log.clear()
        val thrown = runCatching {
            InviteRestoreOrder.run({ log += "consume"; true }, { log += "send"; error("offline") }, { log += "restore" })
        }.exceptionOrNull()
        assertEquals("offline", thrown?.message)
        assertEquals(listOf("consume", "send"), log)
    }

    @Test
    fun `a second Add keeps the first Add time`() {
        val list = PendingContacts.adding(
            contact("aa", 1, at = 9_000, name = "Anna B"),
            listOf(contact("aa", 1, at = 1_000)),
        )
        assertEquals(1, list.size)
        assertEquals(1_000L, list[0].addedAtMs)
        assertEquals("Anna B", list[0].displayName)
    }

    @Test
    fun `what is written is keyed on the inviter's account id, a slot no conversation shares`() {
        val hex = "a".repeat(32)
        assertEquals(hex, contact(hex, 1).queueKey)
        assertTrue(me.lakko.app.chat.PendingInvitedQueue.isQueueKey(hex))
        // A conversation id is a 32-byte group id: 64 hex characters.
        assertTrue(!me.lakko.app.chat.PendingInvitedQueue.isQueueKey("b".repeat(64)))
        assertTrue(!me.lakko.app.chat.PendingInvitedQueue.isQueueKey("Z".repeat(32)))
    }

    @Test
    fun `the line changes after seven days`() {
        val c = contact("aa", 1, at = 0)
        assertEquals(PendingContacts.Line.WAITING, PendingContacts.line(c, PendingContacts.STALE_MS - 1))
        assertEquals(PendingContacts.Line.STALE, PendingContacts.line(c, PendingContacts.STALE_MS))
    }

    @Test
    fun `a contact already known is not listed twice and only their one-to-one takes the messages`() {
        val a = contact("aa", 1)
        val b = contact("bb", 2)
        assertEquals(listOf("bb"), PendingContacts.shown(listOf(a, b), setOf(a.signaturePubkeyB64)).map { it.accountIdHex })
        assertEquals("aa", PendingContacts.forConversation(listOf(a, b), listOf(a.signaturePubkeyB64))?.accountIdHex)
        assertNull(PendingContacts.forConversation(listOf(a, b), listOf(base64Encode(ByteArray(32) { 9 }))))
    }

    @Test
    fun `the name is the display name then the username`() {
        assertEquals("Anna", PendingContacts.name(contact("aa", 1)))
        assertEquals("@anna", PendingContacts.name(contact("aa", 1, name = " ")))
        assertNull(PendingContacts.name(contact("aa", 1, name = "", username = "")))
    }

    @Test
    fun `the store keeps an account's list and an empty list removes it`() {
        val blobs = MapBlobs()
        val store = SealedPendingContactsStore(blobs)
        val account = base64Encode(ByteArray(16) { 1 })
        val list = listOf(contact("aa", 1))
        assertTrue(store.save(account, list))
        assertEquals(list, store.load(account))
        assertTrue(store.save(account, emptyList()))
        assertTrue(blobs.map.isEmpty())
        assertTrue(store.load(account).isEmpty())
    }

    private fun record(
        id: Byte,
        uses: UInt = 0u,
        maxUses: UInt = 0u,
        expiry: ULong = 0uL,
        revoked: Boolean = false,
        minted: ULong = 1uL,
        ledger: Boolean = true,
    ) = LinkRecord(
        hlc = 0uL,
        linkId = ByteArray(16) { id },
        linkSecret = ByteArray(12) { 7 },
        kind = LinkRecordKind.CONTACT,
        ledgerId = if (ledger) ByteArray(6) { id } else ByteArray(0),
        revocationSeed = if (ledger) ByteArray(32) { 9 } else ByteArray(0),
        expiryMs = expiry,
        maxUses = maxUses,
        uses = uses,
        revoked = revoked,
        mintedAtMs = minted,
        label = "",
        answered = emptyList(),
    )

    @Test
    fun `the link list shows live contact links newest first`() {
        val records = listOf(
            record(1, minted = 1uL),
            record(2, minted = 3uL),
            record(3, revoked = true),
            record(4, uses = 1u),
            record(5, uses = 1u, maxUses = 5u, minted = 2uL),
            record(6, expiry = 10uL),
            record(7, ledger = false),
        )
        val listed = LinkRecords.listedContactLinks(records, 100uL)
        assertEquals(listOf<Byte>(2, 5, 1), listed.map { it.linkId[0] })
        assertNull(LinkRecords.effectiveLimit(UInt.MAX_VALUE))
        assertEquals(1u, LinkRecords.effectiveLimit(0u))
    }

    @Test
    fun `the invite page offers Open chat for a known contact and one state for every dead link`() {
        assertEquals(
            InviteLandingState.AlreadyContacts(InviteChatTarget.Conversation("ab"), "Anna"),
            InviteLandingViewModel.stateFor(InviteOpenResult.KnownContact("key", "ab", "Anna")),
        )
        assertEquals(
            InviteLandingState.AlreadyContacts(InviteChatTarget.Contact("key"), null),
            InviteLandingViewModel.stateFor(InviteOpenResult.KnownContact("key", null, null)),
        )
        assertEquals(InviteLandingState.Dead, InviteLandingViewModel.stateFor(InviteOpenResult.Dead))
        assertEquals(InviteLandingState.Own, InviteLandingViewModel.stateFor(InviteOpenResult.Own))
        assertEquals(InviteLandingState.OpenFailed, InviteLandingViewModel.stateFor(InviteOpenResult.Failed("x")))
    }

    @Test
    fun `a pending contact matches by stated name or username, and all of them with nothing typed`() {
        val ines = contact("0a", 1, name = "Ines Vik", username = "t4i5066")
        val anna = contact("0b", 2)
        val both = listOf(ines, anna)
        assertEquals(listOf(ines), PendingContacts.matching(both, "ines"))
        assertEquals(listOf(ines), PendingContacts.matching(both, " INES vik "))
        assertEquals(listOf(ines), PendingContacts.matching(both, "@t4i"))
        assertEquals(emptyList<PendingContact>(), PendingContacts.matching(both, "zoe"))
        assertEquals(both, PendingContacts.matching(both, "  "))
    }
}
