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
        )

    private fun message(id: String, text: String) = PendingInvitedMessage(localId = id, queuedAtMs = 1L, text = text)

    @Test
    fun `a second Add keeps the first Add time and the messages`() {
        val first = PendingContacts.queueing(listOf(contact("aa", 1, at = 1_000)), "aa", message("m1", "hi"))
        val list = PendingContacts.adding(contact("aa", 1, at = 9_000, name = "Anna B"), first)
        assertEquals(1, list.size)
        assertEquals(1_000L, list[0].addedAtMs)
        assertEquals("Anna B", list[0].displayName)
        assertEquals(listOf("hi"), list[0].messages.map { it.text })
    }

    @Test
    fun `messages are kept in the order written`() {
        var list = listOf(contact("aa", 1), contact("bb", 2))
        for ((i, text) in listOf("one", "two", "three").withIndex()) {
            list = PendingContacts.queueing(list, "aa", message("m$i", text))
        }
        assertEquals(listOf("one", "two", "three"), list.first { it.accountIdHex == "aa" }.messages.map { it.text })
        assertTrue(list.first { it.accountIdHex == "bb" }.messages.isEmpty())
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
        val list = PendingContacts.queueing(listOf(contact("aa", 1)), "aa", message("m1", "hello"))
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
}
