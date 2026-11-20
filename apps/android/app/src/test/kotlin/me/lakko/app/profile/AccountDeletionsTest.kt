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

        override fun isDeleted(statementVerified: Boolean, lastAnswer: DirectoryAnswer?): Boolean =
            statementVerified || lastAnswer == DirectoryAnswer.ABSENT

        override fun mayShowUsername(deleted: Boolean, confirmedAtMs: Long?, nowMs: Long): Boolean {
            if (deleted || confirmedAtMs == null || confirmedAtMs > nowMs) return false
            return nowMs - confirmedAtMs < VALID_MS
        }

        override fun checkDue(statementVerified: Boolean, lastAnswerAtMs: Long?, lastAttemptAtMs: Long?, nowMs: Long): Boolean {
            if (statementVerified) return false
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
    fun `a verified statement marks the person deleted for good`() {
        assertFalse(book.isDeleted(alice))
        assertTrue(book.markStatementVerified(alice))
        assertTrue(book.isDeleted(alice))
        // No later answer reverses it.
        book.recordAnswer(alice, DirectoryAnswer.SAME_PERSON)
        assertTrue(book.isDeleted(alice))
        assertFalse(book.isDeleted(bob))
        // A second statement is not new.
        assertFalse(book.markStatementVerified(alice))
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
    fun `becoming deleted is told once`() {
        val told = mutableListOf<String>()
        book.onPersonDeleted = { told += it }
        book.recordAnswer(alice, DirectoryAnswer.ABSENT)
        book.recordAnswer(alice, DirectoryAnswer.ABSENT)
        book.markStatementVerified(alice)
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
        // Never after a statement.
        book.markStatementVerified(bob)
        assertFalse(book.beginCheck(bob))
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
    fun `the removal ledger records an owed removal once and retires it when proposed`() {
        val ledger = DeletedMemberRemovals(MemoryLedger())
        val group = "ab".repeat(32)
        assertTrue(ledger.markOwed(group, alice))
        assertFalse(ledger.markOwed(group, alice))
        assertEquals(listOf(alice), ledger.owed(group))
        assertTrue(ledger.owed("cd".repeat(32)).isEmpty())
        assertFalse(ledger.proposed(group, alice))
        ledger.markProposed(group, alice)
        assertTrue(ledger.proposed(group, alice))
        assertTrue(ledger.owed(group).isEmpty())
        // Once proposed it is never owed again.
        assertFalse(ledger.markOwed(group, alice))
    }

    @Test
    fun `a directory Absent marks deleted but is not a verified statement`() {
        // Section 5: only a verified statement makes a device owe a removal
        // (`ChatRepository.receiveAccountDeleted` is the one writer of the
        // ledger). A pull answer leaves `statementVerified` false.
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
    }
}
