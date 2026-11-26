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
import me.lakko.app.reach.continuityDest
import me.lakko.app.reach.WelcomeDelivery
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
        // Profile grants (sender resolved at receipt) and join-attribution
        // reply grants (the joiner's resolved account) go through this one
        // check; a `/c/` link grant is verified in the core.
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
            // ADR-0087 section 15 — the owner's answer to a publisher request:
            // its own event (landed against the pending requests), never a
            // chat Welcome.
            crypto.seal(aliceMailbox, MailboxRowKind.WELCOME_RENDEZVOUS, byteArrayOf(7)),
        )
        val result = contactsClient(rows).use {
            it.pollStrangerInbox(own = FakeOwnMailbox(aliceMailbox), myX25519Secret = ByteArray(32))
        }
        assertEquals(4L, result.cursor)
        val events = result.events
        assertEquals(2, events.size)
        val event = events.filterIsInstance<IncomingMailboxEvent.Welcome>().single()
        assertArrayEquals(welcome, event.welcomeBytes)
        assertEquals(3L, event.messageId)
        val publisher = events.filterIsInstance<IncomingMailboxEvent.PublisherWelcome>().single()
        assertArrayEquals(byteArrayOf(7), publisher.welcomeBytes)
        assertEquals(4L, publisher.messageId)
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

    // ------------------------------------------------------------ ADR-0101 continuity mailbox

    @Test
    fun `the continuity write goes to the kept mailbox only for an accepted reset inside its window`() {
        val accepted = me.lakko.app.contacts.AcceptedResetOfKey(
            accountIdB64 = java.util.Base64.getEncoder().encodeToString(ByteArray(16) { 7 }),
            keyGeneration = 3,
            signedAtMs = 1_000L,
        )
        val target = ByteArray(32) { 0x55 }
        var seedArgs: Pair<ByteArray, Long>? = null
        val seen = ByteArray(32) { 9 }
        val dest = continuityDest(
            accepted = accepted,
            targetPk = target,
            nowMs = 2_000L,
            seedOf = { id, gen -> seedArgs = id to gen; seen.copyOf() },
            signerOf = { DsReachAuth(NoSigner()) },
        )
        assertNotNull(dest)
        assertArrayEquals(target, dest!!.mailboxPk)
        assertEquals(3L, seedArgs!!.second)
        assertArrayEquals(ByteArray(16) { 7 }, seedArgs!!.first)
        // No accepted reset, no kept mailbox, or past the window: nothing.
        assertNull(continuityDest(null, target, 2_000L, { _, _ -> ByteArray(32) }, { DsReachAuth(NoSigner()) }))
        assertNull(continuityDest(accepted, null, 2_000L, { _, _ -> ByteArray(32) }, { DsReachAuth(NoSigner()) }))
        val past = 1_000L + me.lakko.app.reach.CONTINUITY_KEY_TTL_MS
        assertNull(continuityDest(accepted, target, past, { _, _ -> ByteArray(32) }, { DsReachAuth(NoSigner()) }))
    }

    @Test
    fun `a successor Welcome to a person with no verified mailbox writes nothing and is never delivered`() = runTest {
        val reset = ByteArray(32) { 0x22 }
        val own = ByteArray(32) { 0x33 }
        val ownAddress = ByteArray(32) { 0x44 }
        val published = mutableListOf<ByteArray>()
        val outcome = deliverSuccessorWelcomes(
            targets = listOf(own, reset),
            // Only the own account's mailbox is known; no continuity mailbox
            // is kept for the reset person.
            mailboxOf = { key -> if (key.contentEquals(own)) ownAddress else null },
            publish = { published += it },
        )
        assertEquals(WelcomeDelivery.UNREACHABLE, outcome)
        assertTrue(published.isEmpty())
    }

    @Test
    fun `a successor Welcome is delivered once per mailbox when every mailbox is known`() = runTest {
        val reset = ByteArray(32) { 0x22 }
        val own = ByteArray(32) { 0x33 }
        val published = mutableListOf<ByteArray>()
        val outcome = deliverSuccessorWelcomes(
            targets = listOf(own, reset, own),
            mailboxOf = { key -> key.map { (it + 1).toByte() }.toByteArray() },
            publish = { published += it },
        )
        assertEquals(WelcomeDelivery.DELIVERED, outcome)
        assertEquals(2, published.size)
    }

    @Test
    fun `a successor Welcome no grant reaches is unreachable, any other failure propagates`() = runTest {
        val target = ByteArray(32) { 0x22 }
        val outcome = deliverSuccessorWelcomes(
            targets = listOf(target),
            mailboxOf = { it },
            publish = { throw MailboxUnknown() },
        )
        assertEquals(WelcomeDelivery.UNREACHABLE, outcome)
        assertThrows(IllegalStateException::class.java) {
            kotlinx.coroutines.runBlocking {
                deliverSuccessorWelcomes(
                    targets = listOf(target),
                    mailboxOf = { it },
                    publish = { throw IllegalStateException("network") },
                )
            }
        }
    }
}
