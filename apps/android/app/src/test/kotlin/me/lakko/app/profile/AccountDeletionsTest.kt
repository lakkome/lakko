// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import java.util.Base64
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0098 — the per-person record, the directory classification, the name
 * gate and the group-removal decision, on the host JVM.
 *
 * The host suite does not load the native core, so the shared rules of
 * `lakko-contacts::account_deleted` are a fake here ([FakeRules]) written to
 * the ADR's stated semantics (section 3); the core's own tests and the FFI
 * crate's `the_rules_cross_the_boundary_unchanged` pin the real rules. What
 * these tests check is that the client feeds the rules the right record and
 * acts on their answer.
 */
class AccountDeletionsTest {

    private object FakeRules : AccountDeletionRules {
        const val VALID_MS = 21L * 24 * 60 * 60 * 1000
        const val RECHECK_MS = 7L * 24 * 60 * 60 * 1000
        const val RETRY_MS = 60L * 60 * 1000

        // `deletionEffects` as the core states it: the display on either
        // input unless the latest answer is SamePerson; the destructive
        // effects only on a statement AND an Absent answer.
        override fun effects(statementVerified: Boolean, lastAnswer: DirectoryAnswer?): DeletionEffects {
            val absent = lastAnswer == DirectoryAnswer.ABSENT
            val both = statementVerified && absent
            return DeletionEffects(
                showDeleted = lastAnswer != DirectoryAnswer.SAME_PERSON && (statementVerified || absent),
                removeFromGroups = both,
                dropHeldSends = both,
            )
        }

        override fun mayShowUsername(deleted: Boolean, confirmedAtMs: Long?, nowMs: Long): Boolean {
            if (deleted || confirmedAtMs == null || confirmedAtMs > nowMs) return false
            return nowMs - confirmedAtMs < VALID_MS
        }

        override fun checkDue(statementVerified: Boolean, lastAnswerAtMs: Long?, lastAttemptAtMs: Long?, nowMs: Long): Boolean {
            if (lastAttemptAtMs != null && lastAttemptAtMs <= nowMs && nowMs - lastAttemptAtMs < RETRY_MS) return false
            return lastAnswerAtMs == null || lastAnswerAtMs > nowMs || nowMs - lastAnswerAtMs >= RECHECK_MS
        }
    }

    private class MemoryPersistence(var account: String? = "acct") : PersonDeletionPersistence {
        val rows = HashMap<String, Map<String, PersonDeletionRecord>>()
        override fun currentAccount(): String? = account
        override fun load(account: String): Map<String, PersonDeletionRecord> = rows[account] ?: emptyMap()
        override fun save(account: String, records: Map<String, PersonDeletionRecord>) {
            rows[account] = records
        }
    }

    private var now = 1_000_000_000_000L
    private val persistence = MemoryPersistence()
    private val book = PersonDeletionBook(persistence, FakeRules, nowMs = { now })

    private val alice = Base64.getEncoder().encodeToString(ByteArray(32) { 1 })
    private val bob = Base64.getEncoder().encodeToString(ByteArray(32) { 2 })

    // ---- the record --------------------------------------------------------

    @Test
    fun `a verified statement shows the person deleted until a later SamePerson answer`() {
        assertFalse(book.isDeleted(alice))
        assertTrue(book.markStatementVerified(alice))
        assertTrue(book.isDeleted(alice))
        // A statement alone is not destructive.
        assertEquals(DeletionEffects(showDeleted = true, removeFromGroups = false, dropHeldSends = false), book.effects(alice))
        // A later confirmation shows the person again (the composer comes back).
        book.recordAnswer(alice, DirectoryAnswer.SAME_PERSON)
        assertFalse(book.isDeleted(alice))
        assertFalse(book.isDeleted(bob))
        // A second statement is not new and does not undo the later answer.
        assertFalse(book.markStatementVerified(alice))
        assertFalse(book.isDeleted(alice))
    }

    @Test
    fun `recording a statement clears the earlier answer, so only an answer after it counts`() {
        book.recordAnswer(alice, DirectoryAnswer.SAME_PERSON)
        assertFalse(book.isDeleted(alice))
        assertTrue(book.markStatementVerified(alice))
        assertNull(book.record(alice).lastAnswer)
        assertNull(book.record(alice).lastAnswerAtMs)
        assertTrue("the earlier SamePerson no longer hides it", book.isDeleted(alice))
        // An Absent from before the statement cannot make it destructive either.
        book.recordAnswer(bob, DirectoryAnswer.ABSENT)
        book.markStatementVerified(bob)
        assertFalse(book.effects(bob).removeFromGroups)
        assertFalse(book.effects(bob).dropHeldSends)
    }

    @Test
    fun `removal and dropping held sends need the statement AND a later 404`() {
        val told = mutableListOf<Pair<String, DeletionEffects>>()
        book.onDeletionEffects = { key, effects -> told += key to effects }
        // A 404 alone: shown deleted, nothing destructive.
        book.recordAnswer(alice, DirectoryAnswer.ABSENT)
        assertTrue(book.isDeleted(alice))
        assertTrue(book.removalOwedKeys().isEmpty())
        // A statement alone: the same.
        book.markStatementVerified(bob)
        assertTrue(book.removalOwedKeys().isEmpty())
        assertTrue(told.isEmpty())
        // The statement, then a 404: both destructive effects, told once.
        book.recordAnswer(bob, DirectoryAnswer.ABSENT)
        assertEquals(listOf(bob), book.removalOwedKeys())
        assertEquals(listOf(bob to DeletionEffects(showDeleted = true, removeFromGroups = true, dropHeldSends = true)), told)
        book.recordAnswer(bob, DirectoryAnswer.ABSENT)
        assertEquals(1, told.size)
        // A later SamePerson takes it back: nothing more is owed.
        book.recordAnswer(bob, DirectoryAnswer.SAME_PERSON)
        assertTrue(book.removalOwedKeys().isEmpty())
        assertFalse(book.isDeleted(bob))
    }

    @Test
    fun `a statement asks the directory at once, past the weekly schedule and the retry window`() {
        var fetches = 0
        val checks = AccountDeletionChecks(
            book = book,
            accountIdOf = { ByteArray(16) { 5 } },
            heldKeysOf = { setOf(alice) },
            fetch = { fetches++; null },
            verifyLink = { _, _ -> null },
            scope = CoroutineScope(Dispatchers.Unconfined),
        )
        // A recent check: the schedule would not ask again now.
        assertTrue(book.beginCheck(alice))
        book.endCheck(alice)
        assertFalse(book.beginCheck(alice))
        assertTrue(checks.onStatementVerified(alice))
        assertEquals(1, fetches)
        // The answer (a 404) reaches the book through the feed.
        checks.onDirectoryAnswer(ByteArray(16) { 5 }, null, null)
        assertTrue(book.effects(alice).removeFromGroups)
        // A repeated statement asks nothing more.
        assertFalse(checks.onStatementVerified(alice))
        assertEquals(1, fetches)
    }

    @Test
    fun `a directory Absent marks deleted, OtherKey never does, SamePerson after Absent reverts`() {
        book.recordAnswer(alice, DirectoryAnswer.OTHER_KEY)
        assertFalse(book.isDeleted(alice))
        book.recordAnswer(alice, DirectoryAnswer.ABSENT)
        assertTrue(book.isDeleted(alice))
        now += 1_000
        book.recordAnswer(alice, DirectoryAnswer.SAME_PERSON)
        assertFalse(book.isDeleted(alice))
        assertEquals(now, book.record(alice).lastSamePersonAtMs)
    }

    @Test
    fun `the record survives a reload from the store`() {
        book.markStatementVerified(alice)
        book.recordAnswer(bob, DirectoryAnswer.ABSENT)
        val reloaded = PersonDeletionBook(persistence, FakeRules, nowMs = { now })
        assertTrue(reloaded.isDeleted(alice))
        assertTrue(reloaded.isDeleted(bob))
    }

    @Test
    fun `nothing is recorded with no account signed in`() {
        persistence.account = null
        assertFalse(book.markStatementVerified(alice))
        assertFalse(book.isDeleted(alice))
    }

    @Test
    fun `a display change alone is never told as a destructive effect`() {
        val told = mutableListOf<String>()
        book.onDeletionEffects = { key, _ -> told += key }
        book.recordAnswer(alice, DirectoryAnswer.ABSENT)
        book.recordAnswer(alice, DirectoryAnswer.ABSENT)
        assertTrue(told.isEmpty())
        // The statement clears the 404 recorded before it: still nothing.
        book.markStatementVerified(alice)
        assertTrue(told.isEmpty())
        book.recordAnswer(alice, DirectoryAnswer.ABSENT)
        assertEquals(listOf(alice), told)
    }

    @Test
    fun `a username is shown only within 21 days of a SamePerson answer`() {
        assertFalse(book.mayShowUsername(alice))
        book.recordAnswer(alice, DirectoryAnswer.SAME_PERSON)
        assertTrue(book.mayShowUsername(alice))
        now += FakeRules.VALID_MS
        assertFalse(book.mayShowUsername(alice))
        // An OtherKey answer confirms nothing.
        book.recordAnswer(bob, DirectoryAnswer.OTHER_KEY)
        assertFalse(book.mayShowUsername(bob))
    }

    @Test
    fun `a check is due once, one in flight, and not again within the retry window`() {
        assertTrue(book.beginCheck(alice))
        assertEquals(now, book.record(alice).lastAttemptAtMs)
        // In flight, and within the retry window: no second request.
        assertFalse(book.beginCheck(alice))
        book.endCheck(alice)
        assertFalse(book.beginCheck(alice))
        now += FakeRules.RETRY_MS
        assertTrue(book.beginCheck(alice))
        book.endCheck(alice)
        // Still checked after a statement.
        book.markStatementVerified(bob)
        assertTrue(book.beginCheck(bob))
        book.endCheck(bob)
    }

    // ---- classification ----------------------------------------------------

    private val conversationKey = ByteArray(32) { 9 }
    private val conversationKeyB64 = Base64.getEncoder().encodeToString(conversationKey)
    private val currentKeyB64 = Base64.getEncoder().encodeToString(ByteArray(32) { 8 })
    private val linkB64 = Base64.getEncoder().encodeToString(byteArrayOf(1, 2, 3))

    @Test
    fun `a 404 is Absent`() {
        assertEquals(DirectoryAnswer.ABSENT, classifyDirectoryAnswer(null, null, setOf(alice)) { _, _ -> null })
    }

    @Test
    fun `the held key served is SamePerson`() {
        assertEquals(DirectoryAnswer.SAME_PERSON, classifyDirectoryAnswer(alice, null, setOf(alice)) { _, _ -> null })
    }

    @Test
    fun `a verified link naming the held key is SamePerson, otherwise OtherKey`() {
        assertEquals(
            DirectoryAnswer.SAME_PERSON,
            classifyDirectoryAnswer(currentKeyB64, linkB64, setOf(conversationKeyB64)) { _, _ -> conversationKey },
        )
        assertEquals(
            DirectoryAnswer.OTHER_KEY,
            classifyDirectoryAnswer(currentKeyB64, linkB64, setOf(conversationKeyB64)) { _, _ -> null },
        )
        assertEquals(
            DirectoryAnswer.OTHER_KEY,
            classifyDirectoryAnswer(currentKeyB64, null, setOf(conversationKeyB64)) { _, _ -> conversationKey },
        )
    }

    @Test
    fun `an answer is recorded for every key held under the account id and for nobody else`() {
        val checks = AccountDeletionChecks(
            book = book,
            accountIdOf = { null },
            heldKeysOf = { hex -> if (hex == "05".repeat(16)) setOf(alice) else emptySet() },
            fetch = { null },
            verifyLink = { _, _ -> null },
            scope = CoroutineScope(Dispatchers.Unconfined),
        )
        checks.onDirectoryAnswer(ByteArray(16) { 5 }, null, null)
        assertTrue(book.isDeleted(alice))
        checks.onDirectoryAnswer(ByteArray(16) { 6 }, null, null)
        assertFalse(book.isDeleted(bob))
    }

    @Test
    fun `an on-screen person is asked about once while the answer is pending`() {
        var fetches = 0
        val checks = AccountDeletionChecks(
            book = book,
            accountIdOf = { ByteArray(16) { 5 } },
            heldKeysOf = { setOf(alice) },
            fetch = { fetches++; null },
            verifyLink = { _, _ -> null },
            scope = CoroutineScope(Dispatchers.Unconfined),
        )
        checks.onScreen(alice)
        checks.onScreen(alice)
        assertEquals(1, fetches)
    }

    @Test
    fun `a network failure records nothing`() {
        val checks = AccountDeletionChecks(
            book = book,
            accountIdOf = { ByteArray(16) { 5 } },
            heldKeysOf = { setOf(alice) },
            fetch = { throw java.io.IOException("offline") },
            verifyLink = { _, _ -> null },
            scope = CoroutineScope(Dispatchers.Unconfined),
        )
        checks.onScreen(alice)
        assertNull(book.record(alice).lastAnswer)
        assertFalse(book.isDeleted(alice))
    }

    @Test
    fun `the own account is never asked about`() {
        var fetches = 0
        val checks = AccountDeletionChecks(
            book = book,
            accountIdOf = { ByteArray(16) { 5 } },
            heldKeysOf = { setOf(alice) },
            fetch = { fetches++; null },
            verifyLink = { _, _ -> null },
            scope = CoroutineScope(Dispatchers.Unconfined),
            ownAccountIdHex = { "05".repeat(16) },
        )
        checks.onScreen(alice)
        checks.onDirectoryAnswer(ByteArray(16) { 5 }, null, null)
        assertEquals(0, fetches)
        assertFalse(book.isDeleted(alice))
    }

    // ---- the name gate -----------------------------------------------------

    private fun directory(): ProfileDirectory {
        val profiles = InMemoryProfilePersistence()
        val dir = ProfileDirectory(
            persistence = profiles,
            scope = CoroutineScope(Dispatchers.Unconfined),
            isNewer = { a, b -> a > b },
            passesRule = { true },
            initialRenames = mapOf(alice to "My Alice"),
        )
        dir.profiles.offer(alice, PeerProfile(displayName = "Alice", username = "alice", updatedAtMs = 1uL))
        dir.verifications.put(alice, "alice", UsernameVerification.VERIFIED)
        dir.attachDeletions(
            AccountDeletionChecks(
                book = book,
                accountIdOf = { null },
                heldKeysOf = { emptySet() },
                fetch = { null },
                verifyLink = { _, _ -> null },
                scope = CoroutineScope(Dispatchers.Unconfined),
            ),
            ownAccountIdHex = { null },
        )
        return dir
    }

    @Test
    fun `a deleted person is Deleted account whatever else is known, a rename included`() {
        val dir = directory()
        book.markStatementVerified(alice)
        val name = dir.resolve(alice)
        assertTrue(name.deleted)
        assertEquals(DELETED_ACCOUNT_NAME, name.primary)
        assertNull(name.verifiedUsername)
        assertNull(name.theirName)
        assertTrue(dir.isDeleted(alice))
    }

    @Test
    fun `a verified username is hidden until the account was confirmed, shown within 21 days`() {
        val dir = directory()
        assertNull(dir.resolve(alice).verifiedUsername)
        assertEquals("My Alice", dir.resolve(alice).primary)
        book.recordAnswer(alice, DirectoryAnswer.SAME_PERSON)
        assertEquals("alice", dir.resolve(alice).verifiedUsername)
        now += FakeRules.VALID_MS
        assertNull(dir.resolve(alice).verifiedUsername)
    }

    // ---- group removal (section 5) ------------------------------------------

    @Test
    fun `an owner or admin proposes removal once, never in a one-to-one, never for the owner`() {
        assertTrue(deletedMemberRemovalDue(isGroup = true, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = false, alreadyProposed = false))
        assertFalse(deletedMemberRemovalDue(isGroup = true, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = false, alreadyProposed = true))
        assertFalse(deletedMemberRemovalDue(isGroup = false, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = false, alreadyProposed = false))
        assertFalse(deletedMemberRemovalDue(isGroup = true, ownRoleIsOwnerOrAdmin = false, deletedIsOwner = false, alreadyProposed = false))
        assertFalse(deletedMemberRemovalDue(isGroup = true, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = true, alreadyProposed = false))
    }

    private class MemoryLedger : DeletedMemberRemovals.Storage {
        var rows: Map<String, Boolean> = emptyMap()
        override fun load(): Map<String, Boolean> = rows
        override fun save(rows: Map<String, Boolean>) {
            this.rows = rows
        }
    }

    @Test
    fun `the removal ledger owes a verified person in every group until proposed there`() {
        val ledger = DeletedMemberRemovals(MemoryLedger())
        val group = "ab".repeat(32)
        val other = "cd".repeat(32)
        assertEquals(listOf(alice), ledger.owed(group, listOf(alice)))
        assertEquals(listOf(alice), ledger.owed(other, listOf(alice)))
        assertTrue(ledger.owed(group, emptyList()).isEmpty())
        assertFalse(ledger.proposed(group, alice))
        ledger.markProposed(group, alice)
        assertTrue(ledger.proposed(group, alice))
        assertTrue(ledger.owed(group, listOf(alice)).isEmpty())
        // Once per group: still owed in the other one.
        assertEquals(listOf(alice), ledger.owed(other, listOf(alice)))
    }

    /** Seen live 2026-11-20 (walk 12): a statement verified in a one-to-one
     *  leads to a removal proposal in a separate group where this device is
     *  owner and the person is a member; a group where this device is a plain
     *  member gets none; a group already proposed gets none. */
    @Test
    fun `a statement in a one-to-one leads to a removal proposal in another group`() {
        assertTrue(book.markStatementVerified(alice))
        assertTrue("not before the directory confirms", book.removalOwedKeys().isEmpty())
        book.recordAnswer(alice, DirectoryAnswer.ABSENT)
        val verified = book.removalOwedKeys()
        assertEquals(listOf(alice), verified)
        val ledger = DeletedMemberRemovals(MemoryLedger())
        val owned = "ab".repeat(32)
        val memberOnly = "cd".repeat(32)

        // Owned group: alice on leaves 3 and 5.
        assertEquals(listOf(alice), ledger.owed(owned, verified))
        assertEquals(
            DeletedRemovalPlan.Propose(3u),
            deletedRemovalPlan(
                isGroup = true, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = false,
                personLeaves = listOf(3u, 5u), pendingRemoveLeaves = emptySet(),
            ),
        )
        // A group where this device is a plain member: nothing, asked again later.
        assertEquals(listOf(alice), ledger.owed(memberOnly, verified))
        assertEquals(
            DeletedRemovalPlan.Wait,
            deletedRemovalPlan(
                isGroup = true, ownRoleIsOwnerOrAdmin = false, deletedIsOwner = false,
                personLeaves = listOf(4u), pendingRemoveLeaves = emptySet(),
            ),
        )
        // Already proposed in the owned group: no longer owed there.
        ledger.markProposed(owned, alice)
        assertTrue(ledger.owed(owned, verified).isEmpty())
        assertEquals(listOf(alice), ledger.owed(memberOnly, verified))
        // Every leaf already has a remove pending: no second proposal.
        assertEquals(
            DeletedRemovalPlan.Wait,
            deletedRemovalPlan(
                isGroup = true, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = false,
                personLeaves = listOf(3u, 5u), pendingRemoveLeaves = setOf(3u, 5u),
            ),
        )
        assertEquals(
            DeletedRemovalPlan.Propose(3u),
            deletedRemovalPlan(
                isGroup = true, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = false,
                personLeaves = listOf(3u, 5u), pendingRemoveLeaves = setOf(3u),
            ),
        )
        // Never the owner (C-63), never a one-to-one, nothing once gone.
        assertEquals(
            DeletedRemovalPlan.Settled,
            deletedRemovalPlan(
                isGroup = true, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = true,
                personLeaves = listOf(0u), pendingRemoveLeaves = emptySet(),
            ),
        )
        assertEquals(
            DeletedRemovalPlan.Settled,
            deletedRemovalPlan(
                isGroup = false, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = false,
                personLeaves = listOf(1u), pendingRemoveLeaves = emptySet(),
            ),
        )
        assertEquals(
            DeletedRemovalPlan.Settled,
            deletedRemovalPlan(
                isGroup = true, ownRoleIsOwnerOrAdmin = true, deletedIsOwner = false,
                personLeaves = emptyList(), pendingRemoveLeaves = emptySet(),
            ),
        )
    }

    @Test
    fun `a directory Absent marks deleted but is not a verified statement`() {
        // Section 5: a pull answer alone never makes a device owe a removal
        // (`removalOwedKeys` needs a verified statement too, written only by
        // `ChatRepository.receiveAccountDeleted`).
        val checks = AccountDeletionChecks(
            book = book,
            accountIdOf = { null },
            heldKeysOf = { setOf(alice) },
            fetch = { null },
            verifyLink = { _, _ -> null },
            scope = CoroutineScope(Dispatchers.Unconfined),
        )
        checks.onDirectoryAnswer(ByteArray(16) { 5 }, null, null)
        assertTrue(book.isDeleted(alice))
        assertFalse(book.record(alice).statementVerified)
        assertTrue(book.removalOwedKeys().isEmpty())
    }

    // ---- a group member who is not a contact (ADR-0098 section 2) ----------

    @Test
    fun `a member row is rekeyed when the profile account id arrives`() {
        var claimed: String? = null
        val before = onScreenCheckKey(bob) { claimed }
        claimed = "05".repeat(16)
        val after = onScreenCheckKey(bob) { claimed }
        assertNull(before?.profileAccountIdHex)
        assertEquals("05".repeat(16), after?.profileAccountIdHex)
        assertTrue("a new key re-runs the row's check", before != after)
        assertNull(onScreenCheckKey(null) { claimed })
        assertNull(onScreenCheckKey("") { claimed })
    }

    @Test
    fun `a member with no account id yet is asked about once the id is known`() {
        var id: ByteArray? = null
        var fetches = 0
        val checks = AccountDeletionChecks(
            book = book,
            accountIdOf = { id },
            heldKeysOf = { setOf(bob) },
            fetch = { fetches++; bob to null },
            verifyLink = { _, _ -> null },
            scope = CoroutineScope(Dispatchers.Unconfined),
        )
        checks.onScreen(bob)
        assertEquals(0, fetches)
        assertNull("nothing recorded without an account id", book.record(bob).lastAttemptAtMs)
        id = ByteArray(16) { 5 }
        checks.onScreen(bob)
        assertEquals(1, fetches)
        // The answer reaches the book through the feed every fetch reports to.
        checks.onDirectoryAnswer(ByteArray(16) { 5 }, bob, null)
        assertEquals(DirectoryAnswer.SAME_PERSON, book.record(bob).lastAnswer)
        assertTrue(book.mayShowUsername(bob))
    }

    @Test
    fun `the directory reads a member's account id from their profile`() {
        val dir = ProfileDirectory(
            persistence = InMemoryProfilePersistence(),
            scope = CoroutineScope(Dispatchers.Unconfined),
            isNewer = { a, b -> a > b },
            passesRule = { true },
        )
        assertNull(dir.accountIdHexOf(bob))
        dir.profiles.offer(bob, PeerProfile(displayName = "Bob", username = "bob", updatedAtMs = 1uL, accountIdHex = "05".repeat(16)))
        assertEquals("05".repeat(16), dir.accountIdHexOf(bob))
    }
}
