// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0045 S7 residual 1 + 2 — Android mirror of the web reference
 * `apps/web/src/chat/unapplied-requests.test.ts` and the two residual-2 helpers
 * in `PendingRequests.kt`.
 *
 * A Compose screen cannot be asserted on here, which is the same split
 * `PendingRequestsTest` already uses: every rule lives in the plain module.
 *
 * The measured discrimination evidence for each rule (each probe was RUN, the
 * failure observed, and the probe reverted) is recorded beside the test.
 */
class UnappliedRequestsTest {
    init {
        EnglishForTests.install()
    }


    private val account = "acct"
    private val group = "grp"
    private val joiner = "am9pbmVyLWtleQ=="
    private val contact = "Y29udGFjdC1rZXk="

    /** In-memory [UnappliedRequestStore] — the same shape
     *  `PendingRequestsTest` uses for its own store. */
    private class MemoryStore : UnappliedRequestStore {
        private val rows = mutableMapOf<String, Map<String, UnappliedRow>>()
        private fun k(a: String, g: String) = "$a/$g"
        override fun load(accountIdB64: String, groupIdHex: String) =
            rows[k(accountIdB64, groupIdHex)] ?: emptyMap()

        override fun save(
            accountIdB64: String,
            groupIdHex: String,
            rows: Map<String, UnappliedRow>,
        ) {
            this.rows[k(accountIdB64, groupIdHex)] = rows
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.keys.filter { it.startsWith("$accountIdB64/") }.forEach { rows.remove(it) }
        }

        override fun clearForGroup(accountIdB64: String, groupIdHex: String) {
            rows.remove(k(accountIdB64, groupIdHex))
        }
    }

    // -----------------------------------------------------------------------
    // Copy
    // -----------------------------------------------------------------------

    private val joinItem = UnappliedRequest(joiner, UnappliedKind.JOIN_APPROVE)
    private val addItem = UnappliedRequest(contact, UnappliedKind.ADD)

    /**
     * ADR-0050 section 2.5a point 5 — every kind has a line, and only the join
     * line names an action: the others are listed because the automatic
     * re-request STOPPED, often because this device lost the role the request
     * needs, so "you can do it again" would be untrue as often as not.
     */
    @Test
    fun `is exactly these strings`() {
        assertEquals("Not applied. Ask them to request to join again.", unappliedLine(joinItem).en)
        assertEquals("Not added.", unappliedLine(addItem).en)
        assertEquals("Not removed.", unappliedLine(UnappliedRequest(contact, UnappliedKind.REMOVE)).en)
        assertEquals(
            "Not applied. You are still a member.",
            unappliedLine(UnappliedRequest(contact, UnappliedKind.LEAVE)).en,
        )
        assertEquals("Not made an admin.", unappliedLine(UnappliedRequest(contact, UnappliedKind.MAKE_ADMIN)).en)
        assertEquals("Still an admin.", unappliedLine(UnappliedRequest(contact, UnappliedKind.REMOVE_ADMIN)).en)
        assertEquals(
            "Ownership was not transferred.",
            unappliedLine(UnappliedRequest(contact, UnappliedKind.TRANSFER_OWNERSHIP)).en,
        )
    }

    /**
     * The rule the whole item turns on, asserted independently of the literals
     * so it survives a re-wording: state that it did not apply, never why. The
     * core reports no reason for an exclusion, so neither may the UI.
     */
    @Test
    fun `never states a reason`() {
        val banned = Regex("expired|refused|rejected|invalid|failed|error|because|permission|allowed")
        val lines = UnappliedKind.values().map { unappliedLine(UnappliedRequest(contact, it)) } + listOf(
            UNAPPLIED_EXPLANATION,
            JOIN_WAIT_PRIMARY,
        )
        for (s in lines) {
            assertTrue("states a reason: ${s.en}", !banned.containsMatchIn(s.en.lowercase()))
        }
    }

    @Test
    fun `the heading counts`() {
        assertEquals("Not applied (2)", unappliedHeading(2).en)
    }

    /**
     * The joiner cannot be TOLD anything: the wait line says the request went
     * out and when an admin sees it, and nothing more (UX 6.2, 2026-11-12).
     */
    @Test
    fun `the joiner is told only that the request went out`() {
        assertEquals(
            "Request sent. An admin will see it when they next open Lakko.",
            JOIN_WAIT_PRIMARY.en,
        )
    }

    // -----------------------------------------------------------------------
    // The store
    // -----------------------------------------------------------------------

    @Test
    fun `round-trips and is idempotent`() {
        val store = MemoryStore()
        recordUnapplied(store, account, group, listOf(joinItem))
        recordUnapplied(store, account, group, listOf(joinItem))
        assertEquals(listOf(joinItem), loadUnapplied(store, account, group))
    }

    /**
     * DISCRIMINATION (RUN, then reverted): with `dismissUnapplied` an early
     * `return`, this and the resurrection test both went red:
     *
     *     java.lang.AssertionError: expected:<1> but was:<2>
     *     java.lang.AssertionError: expected:<0> but was:<1>
     */
    @Test
    fun `dismisses one row without touching the other`() {
        val store = MemoryStore()
        recordUnapplied(
            store, account, group,
            listOf(joinItem, addItem),
        )
        dismissUnapplied(store, account, group, joinItem.rowKey)
        val left = loadUnapplied(store, account, group)
        assertEquals(1, left.size)
        assertEquals(contact, left[0].subjectKeyB64)
    }

    /**
     * A dismissed row must STAY dismissed: the detection re-runs on every fold,
     * and re-recording what the user has already read would make the notice
     * un-dismissable.
     *
     * DISCRIMINATION (RUN, then reverted): with `recordUnapplied` overwriting a
     * dismissed row instead of honouring the tombstone:
     *
     *     java.lang.AssertionError: expected:<0> but was:<1>
     */
    @Test
    fun `does not resurrect a dismissed row`() {
        val store = MemoryStore()
        recordUnapplied(store, account, group, listOf(joinItem))
        dismissUnapplied(store, account, group, joinItem.rowKey)
        recordUnapplied(store, account, group, listOf(joinItem))
        assertEquals(0, loadUnapplied(store, account, group).size)
    }

    @Test
    fun `is scoped per account and per group`() {
        val store = MemoryStore()
        recordUnapplied(store, account, group, listOf(joinItem))
        assertEquals(0, loadUnapplied(store, account, "other").size)
        assertEquals(0, loadUnapplied(store, "other", group).size)
    }

    @Test
    fun `clears one account and leaves the others alone`() {
        val store = MemoryStore()
        recordUnapplied(store, account, group, listOf(joinItem))
        recordUnapplied(store, account, "g2", listOf(addItem))
        recordUnapplied(store, "other", group, listOf(addItem))
        store.clearForAccount(account)
        assertEquals(0, loadUnapplied(store, account, group).size)
        assertEquals(0, loadUnapplied(store, account, "g2").size)
        assertEquals(1, loadUnapplied(store, "other", group).size)
    }

    // -----------------------------------------------------------------------
    // The SharedPreferences row codec
    // -----------------------------------------------------------------------

    /** Two kinds about ONE person are two rows: a stopped promote must not hide
     *  a stopped removal of the same member. */
    @Test
    fun `two kinds about one subject are two rows`() {
        val store = MemoryStore()
        recordUnapplied(
            store, account, group,
            listOf(
                UnappliedRequest(contact, UnappliedKind.REMOVE),
                UnappliedRequest(contact, UnappliedKind.MAKE_ADMIN),
            ),
        )
        assertEquals(2, loadUnapplied(store, account, group).size)
    }

    @Test
    fun `the row codec round-trips kind subject and tombstone`() {
        val rows = mapOf(
            joinItem.rowKey to UnappliedRow(UnappliedKind.JOIN_APPROVE, joiner, dismissed = false),
            "remove:$contact" to UnappliedRow(UnappliedKind.REMOVE, contact, dismissed = true),
        )
        val encoded = SharedPrefsUnappliedRequestStore.encodeRow(rows)
        assertEquals(rows, SharedPrefsUnappliedRequestStore.decodeRow(encoded))
    }

    @Test
    fun `a corrupt row reads as empty rather than throwing`() {
        assertEquals(
            emptyMap<String, UnappliedRow>(),
            SharedPrefsUnappliedRequestStore.decodeRow("garbage|only|two|many|fields\nnot-a-kind|key|0"),
        )
        assertEquals(
            emptyMap<String, UnappliedRow>(),
            SharedPrefsUnappliedRequestStore.decodeRow(null),
        )
        assertEquals(
            emptyMap<String, UnappliedRow>(),
            SharedPrefsUnappliedRequestStore.decodeRow(""),
        )
    }

    /** The row separator must be an ESCAPED NUL, never a raw one: a raw NUL in
     *  a Kotlin source makes git classify the file as binary, which silently
     *  removes it from `grep -r` (ADR-0042 MEDIUM-3, and it has bitten this repo
     *  three times). Asserted on the byte, not on the source. */
    @Test
    fun `the row key separator is a NUL`() {
        val encoded = SharedPrefsUnappliedRequestStore.encodeRow(
            mapOf(joinItem.rowKey to UnappliedRow(UnappliedKind.JOIN_APPROVE, joiner)),
        )
        assertTrue("the VALUE must not carry a NUL", !encoded.contains('\u0000'))
    }
}
