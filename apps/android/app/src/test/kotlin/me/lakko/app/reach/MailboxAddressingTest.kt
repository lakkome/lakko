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

    // --- rule 6: a refused write re-checks the address once ------------------

    private class Refused : IOException("401")

    @Test
    fun `a write that is taken never re-checks`() = runTest {
        var rechecks = 0
        val out = writeRecheckingAddress(write = { "stored" }, isRefusal = { it is Refused }) {
            rechecks += 1
            true
        }
        assertEquals("stored", out)
        assertEquals(0, rechecks)
    }

    @Test
    fun `a refused write re-checks once and is tried once more`() = runTest {
        var writes = 0
        var rechecks = 0
        val out = writeRecheckingAddress(
            write = {
                writes += 1
                if (writes == 1) throw Refused()
                "stored"
            },
            isRefusal = { it is Refused },
            recheck = {
                rechecks += 1
                true
            },
        )
        assertEquals("stored", out)
        assertEquals(2, writes)
        assertEquals(1, rechecks)
    }

    @Test
    fun `a second refusal propagates and never loops`() = runTest {
        var writes = 0
        var rechecks = 0
        val thrown = runCatching {
            writeRecheckingAddress<String>(
                write = {
                    writes += 1
                    throw Refused()
                },
                isRefusal = { it is Refused },
                recheck = {
                    rechecks += 1
                    true
                },
            )
        }.exceptionOrNull()
        assertTrue(thrown is Refused)
        assertEquals(2, writes)
        assertEquals(1, rechecks)
    }

    @Test
    fun `a re-check that could not run leaves the first refusal standing`() = runTest {
        for (recheck in listOf<suspend () -> Boolean>({ false }, { throw IOException("down") })) {
            var writes = 0
            val first = Refused()
            val thrown = runCatching {
                writeRecheckingAddress<String>(
                    write = {
                        writes += 1
                        throw first
                    },
                    isRefusal = { it is Refused },
                    recheck = recheck,
                )
            }.exceptionOrNull()
            assertTrue(thrown === first)
            assertEquals(1, writes)
        }
    }

    @Test
    fun `a failure that is not a refusal is never re-checked`() = runTest {
        var rechecks = 0
        val thrown = runCatching {
            writeRecheckingAddress<String>(
                write = { throw IOException("down") },
                isRefusal = { it is Refused },
                recheck = {
                    rechecks += 1
                    true
                },
            )
        }.exceptionOrNull()
        assertTrue(thrown is IOException && thrown !is Refused)
        assertEquals(0, rechecks)
    }

    @Test
    fun `the address is re-checked at most once per interval`() {
        assertTrue(addressRecheckDue(null, 1_000L))
        assertFalse(addressRecheckDue(1_000L, 1_000L + ADDRESS_RECHECK_INTERVAL_MS - 1))
        assertTrue(addressRecheckDue(1_000L, 1_000L + ADDRESS_RECHECK_INTERVAL_MS))
        // A clock that went backwards re-checks rather than waiting forever.
        assertTrue(addressRecheckDue(5_000L, 4_000L))
    }

    @Test
    fun `a grant that replaces another keeps the replaced one beside it`() {
        val first = heldWithGrant(emptyList(), hex(1), "CONTACT", "", "aa")
        assertEquals(listOf(StoredHeld(hex(1), "CONTACT", "", "aa")), first)
        assertEquals(listOf("aa"), grantWiresOf(first.single()))
        // The same grant again changes nothing (the same list).
        assertTrue(heldWithGrant(first, hex(1), "CONTACT", "", "aa") === first)
        // A newer one: tried first, the old one still a candidate after it.
        val second = heldWithGrant(first, hex(1), "CONTACT", "", "bb")
        assertEquals(listOf("bb", "aa"), grantWiresOf(second.single()))
        // Only the grant just replaced is kept, never a growing tail.
        val third = heldWithGrant(second, hex(1), "CONTACT", "", "cc")
        assertEquals(listOf("cc", "bb"), grantWiresOf(third.single()))
        // Another class, scope or person is another entry.
        val more = heldWithGrant(heldWithGrant(third, hex(1), "GROUP", "g1", "dd"), hex(2), "CONTACT", "", "ee")
        assertEquals(3, more.size)
        assertEquals(listOf("cc", "bb"), grantWiresOf(more.first { it.kind == "CONTACT" && it.ownerKeyHex == hex(1) }))
    }

    @Test
    fun `a known-current address drops the grants kept beside newer ones, for that person only`() {
        val held = listOf(
            StoredHeld(hex(1), "CONTACT", "", "bb", "aa"),
            StoredHeld(hex(2), "CONTACT", "", "dd", "cc"),
            StoredHeld(hex(3), "CONTACT", "", "ee"),
        )
        val after = withoutPreviousGrants(held, listOf(hex(1)))
        assertEquals(listOf("bb"), grantWiresOf(after[0]))
        assertEquals(listOf("dd", "cc"), grantWiresOf(after[1]))
        // Nothing to drop: the same list.
        assertTrue(withoutPreviousGrants(held, listOf(hex(3))) === held)
        assertTrue(withoutPreviousGrants(after, listOf(hex(1))) === after)
    }

    @Test
    fun `a peer that got the new grant first still reaches the old mailbox`() {
        // The re-sent profile arrives before the proof is observed: the new
        // grant is filed under the only key known, the old one kept beside.
        val before = heldWithGrant(listOf(StoredHeld(hex(1), "CONTACT", "", "old")), hex(1), "CONTACT", "", "new")
        assertEquals(listOf("new", "old"), heldForPerson(before, personKeysHex(hex(1), emptyList())).flatMap(::grantWiresOf))
        // Once the step is proven both are still found under the person.
        val links = proven(alice, 1, 2)
        assertEquals(listOf("new", "old"), heldForPerson(before, personKeysHex(hex(2), links)).flatMap(::grantWiresOf))
    }

    // --- committer delegations naming an own predecessor --------------------

    @Test
    fun `a delegation naming any own key is found, the current key first`() {
        val current = key(9)
        val previous = key(8)
        val earliest = key(7)
        val keys = listOf(current, previous, earliest)
        // Envelope "p" names the previous key, "c" the current one.
        val names = mapOf("p" to previous, "c" to current, "x" to key(1))
        val headerFor = { env: String, k: ByteArray -> if (names.getValue(env).contentEquals(k)) "hdr-$env" else null }
        assertEquals("hdr-p" to 1, pickOwnDelegation(listOf("p"), keys, headerFor))
        assertEquals("hdr-c" to 0, pickOwnDelegation(listOf("c", "p"), keys, headerFor))
        // The first envelope that names an own key wins, whichever key.
        assertEquals("hdr-p" to 1, pickOwnDelegation(listOf("x", "p", "c"), keys, headerFor))
        // A delegation to somebody else's key authorises nothing here.
        assertEquals(null, pickOwnDelegation(listOf("x"), keys, headerFor))
        assertEquals(null, pickOwnDelegation(emptyList<String>(), keys, headerFor))
        // A device that holds no predecessor seed finds only its current key.
        assertEquals(null, pickOwnDelegation(listOf("p"), listOf(current), headerFor))
    }

    @Test
    fun `a malformed delegation envelope authorises nothing and hides no other`() {
        val keys = listOf(key(9), key(8))
        val headerFor = { env: String, _: ByteArray ->
            if (env == "bad") throw IllegalStateException("malformed") else "hdr-$env"
        }
        assertEquals("hdr-ok" to 0, pickOwnDelegation(listOf("bad", "ok"), keys, headerFor))
        assertEquals(null, pickOwnDelegation(listOf("bad"), keys, headerFor))
    }

    // --- rule 5: what was granted before the rotation is spent ---------------

    @Test
    fun `every unanswered request is re-sent, replaced, dropped or kept owed`() = runTest {
        val pending = listOf("sent", "gone", "down", "boom")
        val out = resendPendingRequests(pending) { entry ->
            when (entry) {
                "sent" -> RequestResend.SENT to "sent-again"
                "gone" -> RequestResend.GONE to null
                "down" -> RequestResend.FAILED to null
                else -> throw IOException("down")
            }
        }
        // One failure does not stop the rest; a throw is "could not try".
        assertEquals(listOf("sent-again", "down", "boom"), out.pending)
        assertFalse(out.done)
    }

    @Test
    fun `the re-send is done only when nothing failed`() = runTest {
        val all = resendPendingRequests(listOf("a", "b")) { e ->
            if (e == "a") RequestResend.SENT to "a2" else RequestResend.GONE to null
        }
        assertEquals(listOf("a2"), all.pending)
        assertTrue(all.done)
        val none = resendPendingRequests(emptyList<String>()) { RequestResend.FAILED to null }
        assertTrue(none.done)
        assertTrue(none.pending.isEmpty())
    }

    @Test
    fun `the links live when the keys moved become the spent ones, each once`() {
        val a = StoredLink("a1", null, "b1")
        val b = StoredLink("a2", 5L)
        val spent = spentLinksAfterMove(emptyList(), listOf(a, b))
        assertEquals(listOf(a, b), spent)
        // A second rotation adds to what is still to be revoked.
        val c = StoredLink("a3", null, "b3")
        assertEquals(listOf(a, b, c), spentLinksAfterMove(spent, listOf(c, a)))
        // Bounded.
        val many = (0 until MAX_SPENT_LINKS + 5).map { StoredLink("l$it") }
        val kept = spentLinksAfterMove(emptyList(), many)
        assertEquals(MAX_SPENT_LINKS, kept.size)
        assertEquals("l${MAX_SPENT_LINKS + 4}", kept.last().linkIdHex)
    }
}
