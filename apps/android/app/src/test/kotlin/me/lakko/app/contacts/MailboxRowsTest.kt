// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import java.util.Base64
import kotlinx.coroutines.test.runTest
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.DsReachAuth
import me.lakko.app.ds.DsSignerRegistry
import me.lakko.app.reach.continuityWelcomeDest
import me.lakko.app.reach.deliverSuccessorWelcomes
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachGrant

/**
 * ADR-0078 phase 1 - sealed mailbox rows on Android, over a fake core: the
 * kind travels inside the seal, a write is sealed to the mailbox key a
 * binding names only after the binding verified against the account key held
 * for the person, a refused binding writes nothing, and the own mailbox opens
 * every row and dispatches by kind.
 *
 * The fake binding "verifies" against an account key when its first 32 bytes
 * ARE that key, and names the mailbox key in its next 32 bytes; a fake row is
 * `kind ordinal || mailbox key || body`.
 */
class MailboxRowsTest {

    private val alice = ByteArray(32) { 0xA1.toByte() }
    private val mallory = ByteArray(32) { 0x66 }
    private val aliceMailbox = ByteArray(32) { 0x0A }
    private val malloryMailbox = ByteArray(32) { 0x6B }

    private fun binding(account: ByteArray, mailbox: ByteArray): ByteArray =
        account + mailbox + ByteArray(33)

    private fun grant(account: ByteArray, mailbox: ByteArray, seed: Byte = 1) =
        FfiReachGrant(FfiReachClass.CONTACT, 1u, ByteArray(32) { seed }, binding(account, mailbox))

    private class FakeCrypto : MailboxCrypto {
        val sealed = mutableListOf<Triple<ByteArray, MailboxRowKind, ByteArray>>()

        override fun seal(mailboxPk: ByteArray, kind: MailboxRowKind, body: ByteArray): ByteArray {
            sealed += Triple(mailboxPk, kind, body)
            return byteArrayOf(kind.ordinal.toByte()) + mailboxPk + body
        }

        override fun address(mailboxPk: ByteArray): ByteArray = ByteArray(32) { (mailboxPk[it] + 1).toByte() }

        override fun verifyBinding(binding: ByteArray, accountPk: ByteArray): ByteArray {
            if (binding.size != 97 || !binding.copyOfRange(0, 32).contentEquals(accountPk)) throw MailboxBindingRefused()
            return binding.copyOfRange(32, 64)
        }

        override fun grantMailboxPk(grant: FfiReachGrant, accountPk: ByteArray): ByteArray =
            verifyBinding(grant.binding, accountPk)
    }

    /** The own mailbox of [mailbox]: opens a fake row addressed to it. */
    private class FakeOwnMailbox(private val mailbox: ByteArray) : OwnMailbox {
        override val address: ByteArray = ByteArray(32) { (mailbox[it] + 1).toByte() }

        override fun open(row: ByteArray): OpenedMailboxRow? {
            if (row.size < 33) return null
            val kind = MailboxRowKind.entries.getOrNull(row[0].toInt()) ?: return null
            if (!row.copyOfRange(1, 33).contentEquals(mailbox)) return null
            return OpenedMailboxRow(kind, row.copyOfRange(33, row.size))
        }
    }

    private class NoSigner : DsJournalSigner {
        override val epoch: ULong = 0uL
        override val verifyingKey: ByteArray = ByteArray(32)
        override fun sign(method: String, path: String, query: String, body: ByteArray): String = "proof"
        override fun mailboxClaimBody(): String = "{}"
    }

    // ------------------------------------------------------------ sealing

    @Test
    fun `every kind is sealed inside the row, to the destination's key, posted to its address`() {
        val crypto = FakeCrypto()
        val dest = MailboxDest(aliceMailbox, auth = null)
        for (kind in MailboxRowKind.entries) {
            val body = byteArrayOf(9, kind.ordinal.toByte())
            val write = crypto.sealFor(dest, kind, body)
            assertArrayEquals(crypto.address(aliceMailbox), write.address)
            // No clear kind byte is prepended by the client: the row is what
            // the core sealed, and the kind opens from inside it.
            val opened = FakeOwnMailbox(aliceMailbox).open(write.row)
            assertEquals(OpenedMailboxRow(kind, body), opened)
        }
        assertEquals(MailboxRowKind.entries.toList(), crypto.sealed.map { it.second })
        assertTrue(crypto.sealed.all { it.first.contentEquals(aliceMailbox) })
    }

    @Test
    fun `the kind enum maps one to one onto the core's`() {
        for (kind in MailboxRowKind.entries) assertEquals(kind, MailboxRowKind.of(kind.toFfi()))
    }

    // ------------------------------------------------------------ verification before writes

    @Test
    fun `a grant whose binding verifies against the pinned key gives its mailbox and authorisation`() {
        var built = 0
        val dest = verifiedGrantDest(grant(alice, aliceMailbox, seed = 7), alice, FakeCrypto()) { seed ->
            built++
            assertArrayEquals(ByteArray(32) { 7 }, seed)
            DsReachAuth(NoSigner())
        }
        assertNotNull(dest)
        assertArrayEquals(aliceMailbox, dest!!.mailboxPk)
        assertNotNull(dest.auth)
        assertEquals(1, built)
    }

    @Test
    fun `a grant whose binding was signed by someone else writes nothing and builds no signer`() {
        // A co-member handing out its own mailbox under Alice's name.
        var built = 0
        val dest = verifiedGrantDest(grant(mallory, malloryMailbox), alice, FakeCrypto()) {
            built++
            DsReachAuth(NoSigner())
        }
        assertNull(dest)
        assertEquals(0, built)
    }

    @Test
    fun `the first held grant that verifies names the mailbox, refused ones are skipped`() {
        val crypto = FakeCrypto()
        assertArrayEquals(
            aliceMailbox,
            firstVerifiedMailboxPk(listOf(grant(mallory, malloryMailbox), grant(alice, aliceMailbox)), alice, crypto),
        )
        assertNull(firstVerifiedMailboxPk(listOf(grant(mallory, malloryMailbox)), alice, crypto))
        assertNull(firstVerifiedMailboxPk(emptyList(), alice, crypto))
    }

    @Test
    fun `a received grant is kept only when it binds the sender's own mailbox`() {
        // Profile grants (sender resolved at receipt), join-attribution reply
        // grants (the joiner's resolved account) and `/c/` link grants (the
        // capability's owner) all go through this one check.
        val crypto = FakeCrypto()
        assertTrue(grantBindsTo(grant(alice, aliceMailbox), alice, crypto))
        assertFalse(grantBindsTo(grant(mallory, malloryMailbox), alice, crypto))
        assertFalse(grantBindsTo(grant(alice, aliceMailbox).copy(binding = ByteArray(10)), alice, crypto))
    }

    @Test
    fun `a search hit's grant or public-record binding is verified, and a refused one is never redirected`() {
        val crypto = FakeCrypto()
        val authOf = { _: ByteArray -> DsReachAuth(NoSigner()) }
        val decode = { wire: ByteArray -> grant(wire.copyOfRange(0, 32), wire.copyOfRange(32, 64)) }

        // A grant from the search: written under it.
        val viaGrant = foundRequestDest(alice + aliceMailbox, null, alice, crypto, authOf, decode)
        assertArrayEquals(aliceMailbox, viaGrant!!.mailboxPk)
        assertNotNull(viaGrant.auth)

        // A public record's binding: proofless (a mailbox-write token).
        val viaBinding = foundRequestDest(null, binding(alice, aliceMailbox), alice, crypto, authOf, decode)
        assertArrayEquals(aliceMailbox, viaBinding!!.mailboxPk)
        assertNull(viaBinding.auth)

        // Neither: the caller falls back to what this device holds.
        assertNull(foundRequestDest(null, null, alice, crypto, authOf, decode))

        // Handed but not verifying: refused, fail closed.
        assertThrows(MailboxBindingRefused::class.java) {
            foundRequestDest(mallory + malloryMailbox, null, alice, crypto, authOf, decode)
        }
        assertThrows(MailboxBindingRefused::class.java) {
            foundRequestDest(null, binding(mallory, malloryMailbox), alice, crypto, authOf, decode)
        }
    }

    // ------------------------------------------------------------ the own mailbox

    @Test
    fun `only account identities have a mailbox - never a device identity`() {
        val conversation = Any()
        val published = Any()
        assertEquals(listOf(conversation, published), ownMailboxIdentities(conversation, published))
        assertEquals(listOf(published), ownMailboxIdentities(null, published))
        assertEquals(listOf(conversation), ownMailboxIdentities(conversation, conversation))
    }

    @Test
    fun `the co-publisher join takes a Welcome or a rendezvous row and nothing else`() {
        val body = byteArrayOf(1, 2, 3)
        assertArrayEquals(body, welcomeBodyOf(OpenedMailboxRow(MailboxRowKind.WELCOME, body)))
        assertArrayEquals(body, welcomeBodyOf(OpenedMailboxRow(MailboxRowKind.WELCOME_RENDEZVOUS, body)))
        assertNull(welcomeBodyOf(OpenedMailboxRow(MailboxRowKind.CONTACT_REQUEST, body)))
        assertNull(welcomeBodyOf(OpenedMailboxRow(MailboxRowKind.CONTACT_ACK, body)))
        assertNull(welcomeBodyOf(null))
    }

    private fun contactsClient(rows: List<ByteArray>): ContactsClient {
        val messages = rows.mapIndexed { i, row ->
            """{"id":${i + 1},"payload_b64":"${Base64.getEncoder().encodeToString(row)}","created_at":"t$i"}"""
        }.joinToString(",")
        val ds = DsClient(
            baseUrl = "http://test.invalid",
            engine = MockEngine {
                respond(
                    """{"messages":[$messages],"has_more":false}""",
                    HttpStatusCode.OK,
                    headersOf(HttpHeaders.ContentType, "application/json"),
                )
            },
            signers = DsSignerRegistry(),
        )
        return ContactsClient(
            discovery = DiscoveryClient(baseUrl = "http://test.invalid", engine = MockEngine { respond("", HttpStatusCode.OK) }),
            ds = ds,
            pinStore = InMemoryPinStore(),
            transparencyClientFactory = { null },
            mailboxCrypto = FakeCrypto(),
        )
    }

    @Test
    fun `the inbox opens every row, dispatches by kind and drops what does not open`() = runTest {
        val crypto = FakeCrypto()
        val welcome = byteArrayOf(0x57, 1)
        val rows = listOf(
            // Garbage: does not open, dropped (the cursor still moves).
            byteArrayOf(1, 2, 3),
            // Sealed to ANOTHER mailbox: does not open here.
            crypto.seal(malloryMailbox, MailboxRowKind.WELCOME, byteArrayOf(9)),
            // A Welcome for this mailbox.
            crypto.seal(aliceMailbox, MailboxRowKind.WELCOME, welcome),
            // The co-publisher rendezvous is read by the channel join, not here.
            crypto.seal(aliceMailbox, MailboxRowKind.WELCOME_RENDEZVOUS, byteArrayOf(7)),
        )
        val result = contactsClient(rows).use {
            it.pollStrangerInbox(own = FakeOwnMailbox(aliceMailbox), myX25519Secret = ByteArray(32))
        }
        assertEquals(4L, result.cursor)
        val events = result.events
        assertEquals(1, events.size)
        val event = events.single() as IncomingMailboxEvent.Welcome
        assertArrayEquals(welcome, event.welcomeBytes)
        assertEquals(3L, event.messageId)
    }

    @Test
    fun `an ack with no verified destination is refused before anything is composed or sent`() = runTest {
        val request = IncomingContactRequest(
            messageId = 1,
            fromPubkey = alice,
            fromAccountId = ByteArray(16),
            fromPseudonym = ByteArray(32),
            validAfterMs = 0uL,
            intro = null,
            createdAt = "t",
        )
        val error = assertThrows(ContactsError::class.java) {
            kotlinx.coroutines.runBlocking {
                contactsClient(emptyList()).use {
                    it.sendContactAck(ByteArray(32), ByteArray(16), ByteArray(32), request, reach = null)
                }
            }
        }
        assertTrue((error.variant as ContactsError.Variant.Wrapped).cause is MailboxUnknown)
    }

    // ------------------------------------------------------------ ADR-0073 continuity: a recorded no-op

    @Test
    fun `the continuity Welcome writes nothing whether or not a reset was accepted`() {
        assertNull(continuityWelcomeDest(acceptedResetKnown = true))
        assertNull(continuityWelcomeDest(acceptedResetKnown = false))
    }

    @Test
    fun `a successor Welcome skips a person with no verified mailbox and never guesses an address`() = runTest {
        val reset = ByteArray(32) { 0x22 }
        val own = ByteArray(32) { 0x33 }
        val ownAddress = ByteArray(32) { 0x44 }
        val published = mutableListOf<ByteArray>()
        val skipped = deliverSuccessorWelcomes(
            targets = listOf(reset, own, own),
            // Only the own account's mailbox is known; the reset person's
            // new mailbox has no carrier (ADR-0078 open question).
            mailboxOf = { key -> if (key.contentEquals(own)) ownAddress else null },
            publish = { published += it },
        )
        assertEquals(1, skipped)
        assertEquals(1, published.size)
        assertArrayEquals(ownAddress, published.single())
    }
}
