// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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

    private val account = "acct"
    private val group = "grp"
    private val joiner = "am9pbmVyLWtleQ=="
    private val contact = "Y29udGFjdC1rZXk="

    private fun own(joinRequest: Boolean = false) =
        RequestObservation(atMs = 1_000L, own = true, joinRequest = joinRequest)

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
    // Detection
    // -----------------------------------------------------------------------

    /**
     * The whole point. The approving admin's own `add:` observation is gone from
     * the core's live pending set and the joiner is not in the roster, so the
     * request died without a trace anywhere else.
     *
     * DISCRIMINATION (RUN, then reverted): with the roster check inverted
     * (`roster.contains` -> `!roster.contains`) two tests went red, this one
     * with:
     *
     *     java.lang.AssertionError: expected:<1> but was:<0>
     */
    @Test
    fun `reports an own add that left the pending set without reaching the roster`() {
        val out = unappliedAddRequests(
            mapOf("add:$joiner" to own(joinRequest = true)),
            emptyList(),
            emptyList(),
        )
        assertEquals(1, out.size)
        assertEquals(UnappliedAdd(joiner, viaJoinRequest = true), out[0])
    }

    /**
     * The single most important false positive to exclude: the request WORKED.
     *
     * DISCRIMINATION (RUN, then reverted): with the same inversion, this side
     * of the same pair went red:
     *
     *     java.lang.AssertionError: expected:<0> but was:<1>
     */
    @Test
    fun `says nothing when the add landed`() {
        assertEquals(
            0,
            unappliedAddRequests(
                mapOf("add:$joiner" to own(joinRequest = true)),
                emptyList(),
                listOf(joiner),
            ).size,
        )
    }

    @Test
    fun `says nothing while the request is still pending`() {
        assertEquals(
            0,
            unappliedAddRequests(
                mapOf("add:$joiner" to own()),
                listOf("add:$joiner"),
                emptyList(),
            ).size,
        )
    }

    /**
     * UX §10 — a member is never told about someone else's request.
     *
     * DISCRIMINATION (RUN, then reverted): with the `own` guard removed:
     *
     *     java.lang.AssertionError: expected:<0> but was:<1>
     */
    @Test
    fun `ignores a request this device did not make`() {
        assertEquals(
            0,
            unappliedAddRequests(
                mapOf("add:$joiner" to RequestObservation(atMs = 1_000L, own = false)),
                emptyList(),
                emptyList(),
            ).size,
        )
    }

    /**
     * The deliberate scope limit, pinned so nobody widens it by accident.
     *
     * `remove:` and `leave:` name a LEAF and OpenMLS hands a vacated leaf to the
     * next `Add`, so a roster test on one can be wrong in both directions; a
     * `role-change` carries no payload at all. All three are also VISIBLE when
     * they fail. The add is the only request whose failure leaves no trace.
     */
    @Test
    fun `claims nothing about a remove a leave or a role change`() {
        assertEquals(
            0,
            unappliedAddRequests(
                mapOf(
                    "remove:4" to own(),
                    "leave:2" to own(),
                    "role-change" to own(),
                    "update:3" to own(),
                ),
                emptyList(),
                emptyList(),
            ).size,
        )
    }

    /**
     * A torn roster read is UNKNOWN, never "nobody is in the group".
     *
     * DISCRIMINATION (RUN, then reverted): with the `null` guard replaced by
     * `rosterKeysB64 ?: emptyList()`:
     *
     *     java.lang.AssertionError: expected:<0> but was:<1>
     */
    @Test
    fun `claims nothing when the roster could not be read`() {
        assertEquals(
            0,
            unappliedAddRequests(
                mapOf("add:$joiner" to own(joinRequest = true)),
                emptyList(),
                null,
            ).size,
        )
    }

    // -----------------------------------------------------------------------
    // Copy
    // -----------------------------------------------------------------------

    /**
     * DISCRIMINATION (RUN, then reverted): with the join line replaced by the UX
     * spec §7 draft `This request expired before it could be applied. Ask them
     * to request to join again.`:
     *
     *     org.junit.ComparisonFailure: expected:<[Not] applied. Ask them t...>
     *     but was:<[This request expired before it could be] applied. Ask them
     *     t...>
     */
    @Test
    fun `is exactly these two strings`() {
        assertEquals(
            "Not applied. Ask them to request to join again.",
            unappliedLine(UnappliedAdd(joiner, viaJoinRequest = true)),
        )
        assertEquals(
            "Not added. You can add them again.",
            unappliedLine(UnappliedAdd(contact, viaJoinRequest = false)),
        )
    }

    /**
     * The rule the whole item turns on, asserted independently of the literals
     * so it survives a re-wording: state that it did not apply, never why. The
     * core reports no reason for an exclusion, so neither may the UI.
     *
     * DISCRIMINATION (RUN, then reverted): with the same §7 draft substitution
     * this went red on the join line — which is the finding, because "expired"
     * is a REASON this client cannot establish:
     *
     *     java.lang.AssertionError: states a reason: This request expired before
     *     it could be applied. Ask them to request to join again.
     */
    @Test
    fun `never states a reason`() {
        val banned = Regex("expired|refused|rejected|invalid|failed|error|because")
        for (s in listOf(
            unappliedLine(UnappliedAdd(joiner, viaJoinRequest = true)),
            unappliedLine(UnappliedAdd(contact, viaJoinRequest = false)),
            UNAPPLIED_EXPLANATION,
            JOIN_WAIT_PRIMARY,
            JOIN_WAIT_SECONDARY,
            JOIN_REQUEST_AGAIN,
        )) {
            assertTrue("states a reason: $s", !banned.containsMatchIn(s.lowercase()))
        }
    }

    @Test
    fun `the heading counts`() {
        assertEquals("Not applied (2)", unappliedHeading(2))
    }

    /**
     * The joiner cannot be TOLD anything, so what it gets is the truth about the
     * wait plus the one repair available to it.
     *
     * DISCRIMINATION (RUN, then reverted): with `JOIN_WAIT_SECONDARY` reverted
     * to the pre-change copy `Waiting for an admin to approve, then for the
     * change to be applied.`:
     *
     *     java.lang.AssertionError: the joiner is not told the wait is unbounded
     */
    @Test
    fun `the joiner is told the wait is unbounded and offered the one repair`() {
        assertEquals(
            "Request sent. An admin approves it, then the change is applied to the group.",
            JOIN_WAIT_PRIMARY,
        )
        assertTrue(
            "the joiner is not told the wait is unbounded",
            JOIN_WAIT_SECONDARY.contains("time limit"),
        )
        assertTrue(JOIN_WAIT_SECONDARY.contains("send the request again"))
        assertEquals("Request again", JOIN_REQUEST_AGAIN)
    }

    // -----------------------------------------------------------------------
    // The store
    // -----------------------------------------------------------------------

    @Test
    fun `round-trips and is idempotent`() {
        val store = MemoryStore()
        val item = UnappliedAdd(joiner, viaJoinRequest = true)
        recordUnapplied(store, account, group, listOf(item))
        recordUnapplied(store, account, group, listOf(item))
        assertEquals(listOf(item), loadUnapplied(store, account, group))
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
            listOf(
                UnappliedAdd(joiner, viaJoinRequest = true),
                UnappliedAdd(contact, viaJoinRequest = false),
            ),
        )
        dismissUnapplied(store, account, group, joiner)
        val left = loadUnapplied(store, account, group)
        assertEquals(1, left.size)
        assertEquals(contact, left[0].addSignatureKeyB64)
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
        val item = UnappliedAdd(joiner, viaJoinRequest = true)
        recordUnapplied(store, account, group, listOf(item))
        dismissUnapplied(store, account, group, joiner)
        recordUnapplied(store, account, group, listOf(item))
        assertEquals(0, loadUnapplied(store, account, group).size)
    }

    @Test
    fun `is scoped per account and per group`() {
        val store = MemoryStore()
        recordUnapplied(store, account, group, listOf(UnappliedAdd(joiner, true)))
        assertEquals(0, loadUnapplied(store, account, "other").size)
        assertEquals(0, loadUnapplied(store, "other", group).size)
    }

    @Test
    fun `clears one account and leaves the others alone`() {
        val store = MemoryStore()
        recordUnapplied(store, account, group, listOf(UnappliedAdd(joiner, true)))
        recordUnapplied(store, account, "g2", listOf(UnappliedAdd(contact, false)))
        recordUnapplied(store, "other", group, listOf(UnappliedAdd(contact, false)))
        store.clearForAccount(account)
        assertEquals(0, loadUnapplied(store, account, group).size)
        assertEquals(0, loadUnapplied(store, account, "g2").size)
        assertEquals(1, loadUnapplied(store, "other", group).size)
    }

    // -----------------------------------------------------------------------
    // The SharedPreferences row codec
    // -----------------------------------------------------------------------

    @Test
    fun `the row codec round-trips both flags`() {
        val rows = mapOf(
            joiner to UnappliedRow(viaJoinRequest = true, dismissed = false),
            contact to UnappliedRow(viaJoinRequest = false, dismissed = true),
        )
        val encoded = SharedPrefsUnappliedRequestStore.encodeRow(rows)
        assertEquals(rows, SharedPrefsUnappliedRequestStore.decodeRow(encoded))
    }

    @Test
    fun `a corrupt row reads as empty rather than throwing`() {
        assertEquals(
            emptyMap<String, UnappliedRow>(),
            SharedPrefsUnappliedRequestStore.decodeRow("garbage|only|two|many|fields"),
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
            mapOf(joiner to UnappliedRow(viaJoinRequest = true)),
        )
        assertTrue("the VALUE must not carry a NUL", !encoded.contains('\u0000'))
    }

    // -----------------------------------------------------------------------
    // Residual 2 — the queue an approve grows
    // -----------------------------------------------------------------------

    /**
     * DISCRIMINATION (RUN, then reverted): with the threshold changed from
     * `< 2` to `< 1` (in the same pass as the `of 64` probe below, which is why
     * the observed string carries both changes):
     *
     *     java.lang.AssertionError: expected null, but was:<1 of 64 changes are
     *     now waiting to apply.>
     */
    @Test
    fun `the queue line stays silent for one change and counts from two`() {
        assertNull(waitingQueueLine(0))
        assertNull(waitingQueueLine(1))
        assertEquals("2 changes are now waiting to apply.", waitingQueueLine(2))
        assertEquals("64 changes are now waiting to apply.", waitingQueueLine(64))
    }

    /**
     * It states what IS, never a limit — the cap is `pub(crate)` in `lakko-mls`
     * and on neither FFI surface, so this layer has no number to compare against
     * and must not imply one.
     *
     * DISCRIMINATION (RUN, then reverted): with the line changed to
     * `${'$'}pendingCount of 64 changes are now waiting to apply.`:
     *
     *     java.lang.AssertionError: implies a limit: 2 of 64 changes are now
     *     waiting to apply.
     */
    @Test
    fun `the queue line never implies a limit it cannot read`() {
        val banned = Regex("limit|maximum|of \\d+|full|cap")
        for (n in listOf(2, 10, 64, 100)) {
            val line = waitingQueueLine(n).orEmpty()
            assertTrue("implies a limit: $line", !banned.containsMatchIn(line))
        }
    }

    /** The quarantine's one statable property. Not a deadline — a volatility. */
    @Test
    fun `the join-request list is described as unsaved and given no expiry`() {
        assertTrue(JOIN_REQUESTS_NOT_SAVED.contains("not saved"))
        val banned = Regex("expire|expires|expired|days|hours|deadline")
        assertTrue(!banned.containsMatchIn(JOIN_REQUESTS_NOT_SAVED.lowercase()))
    }
}
