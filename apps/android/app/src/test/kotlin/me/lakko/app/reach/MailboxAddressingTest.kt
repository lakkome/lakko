// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.test.runTest
import me.lakko.app.contacts.KeyChangeEvidence
import me.lakko.app.contacts.ProvenLink
import me.lakko.app.contacts.provenLinkFor
import me.lakko.app.contacts.withProvenLink
import me.lakko.app.contacts.withoutLinksOf
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.util.Base64

/**
 * ADR-0066 section 3.5a - the mailbox after a rotation of the account keys:
 * resolve before addressing (proven steps only, chains, the own account),
 * grants found across a proven rotation, the 30-day read rule of a
 * predecessor's mailbox, and the profile re-send that stays owed.
 */
class MailboxAddressingTest {
    private fun key(seed: Int): ByteArray = ByteArray(32) { seed.toByte() }

    private fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)

    private fun hex(seed: Int): String = reachHex(key(seed))

    private val alice = "acct-alice"
    private val me = "acct-me"

    /** The link the gate's verdict earns, added the way the store adds it. */
    private fun List<ProvenLink>.plus(
        evidence: KeyChangeEvidence,
        account: String,
        old: Int,
        new: Int,
    ): List<ProvenLink> {
        val link = provenLinkFor(evidence, account, b64(key(old)), b64(key(new)), 1L) ?: return this
        return withProvenLink(this, link)
    }

    private fun proven(account: String, old: Int, new: Int): List<ProvenLink> =
        emptyList<ProvenLink>().plus(KeyChangeEvidence.ProofVerified, account, old, new)

    // --- rule 1: resolve before addressing ----------------------------------

    @Test
    fun `a key nothing is known about is addressed as itself`() {
        assertEquals(hex(1), addressedKeyHex(hex(1), emptyList()))
        assertEquals(hex(1), addressedKeyHex(hex(1), proven(alice, 7, 8)))
        assertArrayEquals(key(1), addressedKey(key(1), emptyList()))
    }

    @Test
    fun `a proven predecessor is addressed at the current key's mailbox`() {
        val links = proven(alice, 1, 2)
        assertEquals(hex(2), addressedKeyHex(hex(1), links))
        assertArrayEquals(key(2), addressedKey(key(1), links))
        // The current key stays itself.
        assertEquals(hex(2), addressedKeyHex(hex(2), links))
        // Upper-case hex is the same key.
        assertEquals(hex(2), addressedKeyHex(hex(1).uppercase(), links))
    }

    @Test
    fun `an unproven key change is never resolved`() {
        // A reset, a stripped proof and a refused proof create no link, so
        // the old key keeps naming the old mailbox and nothing else.
        for (evidence in listOf(KeyChangeEvidence.Unproven, KeyChangeEvidence.ProofRefused)) {
            val links = emptyList<ProvenLink>().plus(evidence, alice, 1, 2)
            assertTrue(links.isEmpty())
            assertEquals("$evidence", hex(1), addressedKeyHex(hex(1), links))
            assertEquals(listOf(hex(1)), personKeysHex(hex(1), links))
        }
    }

    @Test
    fun `a chain of proven rotations resolves to its newest key from every link`() {
        val links = proven(alice, 1, 2).plus(KeyChangeEvidence.ProofVerified, alice, 2, 3)
        assertEquals(hex(3), addressedKeyHex(hex(1), links))
        assertEquals(hex(3), addressedKeyHex(hex(2), links))
        assertEquals(hex(3), addressedKeyHex(hex(3), links))
    }

    @Test
    fun `a reset after a rotation ends the resolution`() {
        val links = withoutLinksOf(proven(alice, 1, 2), alice)
        assertEquals(hex(1), addressedKeyHex(hex(1), links))
    }

    @Test
    fun `this account's own conversation key resolves to its published key`() {
        // The leaves of this account's devices still name the old key; a
        // Welcome for "another device of my own account" goes to the mailbox
        // of the key the account is at now.
        val links = proven(me, 10, 11).plus(KeyChangeEvidence.ProofVerified, alice, 1, 2)
        assertArrayEquals(key(11), addressedKey(key(10), links))
        // And a contact's step never moves this account's key or the reverse.
        assertArrayEquals(key(2), addressedKey(key(1), links))
        assertArrayEquals(key(11), addressedKey(key(11), links))
    }

    @Test
    fun `one account's proven step cannot redirect another account's key`() {
        // A step that names a key already in another account's history is
        // refused when recorded, so it resolves nothing.
        val links = proven(alice, 1, 2).plus(KeyChangeEvidence.ProofVerified, "acct-mallory", 2, 9)
        assertEquals(hex(2), addressedKeyHex(hex(1), links))
        assertEquals(hex(2), addressedKeyHex(hex(2), links))
    }

    @Test
    fun `input that is not a key is returned unchanged`() {
        assertEquals("zz", addressedKeyHex("zz", proven(alice, 1, 2)))
        assertEquals(listOf("zz"), personKeysHex("zz", proven(alice, 1, 2)))
    }

    // --- rule 2: grants are keyed on the person -----------------------------

    private fun held(owner: Int, kind: ReachKind, grant: String, scope: String = "") =
        StoredHeld(ownerKeyHex = hex(owner), kind = kind.name, scopeHex = scope, grantHex = grant)

    @Test
    fun `a person's keys are listed current first, then each predecessor`() {
        val links = proven(alice, 1, 2).plus(KeyChangeEvidence.ProofVerified, alice, 2, 3)
        val expected = listOf(hex(3), hex(2), hex(1))
        assertEquals(expected, personKeysHex(hex(1), links))
        assertEquals(expected, personKeysHex(hex(2), links))
        assertEquals(expected, personKeysHex(hex(3), links))
        assertEquals(listOf(hex(5)), personKeysHex(hex(5), links))
    }

    @Test
    fun `a grant from a predecessor-bound leaf is filed under the current key`() {
        // The key a grant is stored under is the resolver's answer for the
        // key the sending leaf names.
        val links = proven(alice, 1, 2)
        assertEquals(hex(2), addressedKeyHex(hex(1), links))
        // Found from either key afterwards.
        val doc = listOf(held(2, ReachKind.CONTACT, "aa"))
        assertEquals(doc, heldForPerson(doc, personKeysHex(hex(1), links)))
        assertEquals(doc, heldForPerson(doc, personKeysHex(hex(2), links)))
    }

    @Test
    fun `a grant filed before its key became a proven predecessor is still found`() {
        val stored = listOf(held(1, ReachKind.CONTACT, "aa"), held(9, ReachKind.CONTACT, "ff"))
        // Before the proof: only under the key it was filed under.
        assertEquals(listOf(stored[0]), heldForPerson(stored, personKeysHex(hex(1), emptyList())))
        assertTrue(heldForPerson(stored, personKeysHex(hex(2), emptyList())).isEmpty())
        // After: under the person's current key as well.
        val links = proven(alice, 1, 2)
        assertEquals(listOf(stored[0]), heldForPerson(stored, personKeysHex(hex(2), links)))
        assertEquals(listOf(stored[0]), heldForPerson(stored, personKeysHex(hex(1), links)))
        // Never another person's.
        assertFalse(stored[1] in heldForPerson(stored, personKeysHex(hex(2), links)))
    }

    @Test
    fun `the current key's grant is tried before a predecessor's of the same class`() {
        val links = proven(alice, 1, 2)
        val old = held(1, ReachKind.CONTACT, "aa")
        val new = held(2, ReachKind.CONTACT, "bb")
        assertEquals(listOf(new, old), heldForPerson(listOf(old, new), personKeysHex(hex(1), links)))
    }

    @Test
    fun `an unproven change leaves the old key's grants with the old key only`() {
        val stored = listOf(held(1, ReachKind.CONTACT, "aa"))
        val links = emptyList<ProvenLink>().plus(KeyChangeEvidence.Unproven, alice, 1, 2)
        assertTrue(heldForPerson(stored, personKeysHex(hex(2), links)).isEmpty())
    }

    // --- rule 4: the predecessor's mailbox ----------------------------------

    @Test
    fun `a predecessor's mailbox is read for thirty days after adopting, then left`() {
        val day = 24L * 60 * 60 * 1000
        val adopted = 1_000_000_000_000L
        assertEquals(30 * day, PREDECESSOR_MAILBOX_READ_MS)
        assertTrue(predecessorMailboxIsRead(adopted, adopted))
        assertTrue(predecessorMailboxIsRead(adopted, adopted + 29 * day))
        assertTrue(predecessorMailboxIsRead(adopted, adopted + 30 * day - 1))
        assertFalse(predecessorMailboxIsRead(adopted, adopted + 30 * day))
        assertFalse(predecessorMailboxIsRead(adopted, adopted + 400 * day))
    }

    @Test
    fun `a clock that went backwards still reads, and an unstamped row does not read forever`() {
        val adopted = 1_000_000_000_000L
        assertTrue(predecessorMailboxIsRead(adopted, adopted - 5_000L))
        assertFalse(predecessorMailboxIsRead(0L, adopted))
    }

    // --- rule 3: the profile re-send ----------------------------------------

    @Test
    fun `the re-send is done only when no conversation failed`() = runTest {
        val sent = mutableListOf<String>()
        val done = resendProfileToAll(listOf("a", "b", "c"), onSent = { sent += it }) { group ->
            if (group == "b") ProfileResend.NOTHING_TO_SEND else ProfileResend.SENT
        }
        assertTrue(done)
        assertEquals(listOf("a", "c"), sent)
        assertTrue(resendProfileToAll(emptyList()) { ProfileResend.FAILED })
    }

    @Test
    fun `one conversation that fails keeps the re-send owed and holds no other back`() = runTest {
        val attempted = mutableListOf<String>()
        val sent = mutableListOf<String>()
        val done = resendProfileToAll(listOf("a", "b", "c"), onSent = { sent += it }) { group ->
            attempted += group
            when (group) {
                "a" -> ProfileResend.FAILED
                "b" -> throw IOException("down")
                else -> ProfileResend.SENT
            }
        }
        assertFalse(done)
        assertEquals(listOf("a", "b", "c"), attempted)
        assertEquals(listOf("c"), sent)
    }

    @Test
    fun `a retry sends only where the first run did not land`() = runTest {
        val landed = mutableSetOf<String>()
        var down = true
        suspend fun run(): Pair<Boolean, List<String>> {
            val attempted = mutableListOf<String>()
            val ok = resendProfileToAll(listOf("a", "b").filter { it !in landed }, onSent = { landed += it }) { g ->
                attempted += g
                if (g == "b" && down) ProfileResend.FAILED else ProfileResend.SENT
            }
            return ok to attempted
        }
        assertEquals(false to listOf("a", "b"), run())
        down = false
        assertEquals(true to listOf("b"), run())
    }
}
