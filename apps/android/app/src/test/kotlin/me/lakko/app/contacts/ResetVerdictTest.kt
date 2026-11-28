// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.transparency.LogVerifyStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0073 R.2 - the acceptance rule, the binding rule and the re-pin plan,
 *  row by row (mirror of the web `reset-verdict` / `verified-binding` /
 *  `local-store` tables). */
class ResetVerdictTest {
    @Test
    fun `the acceptance table is the core's - no copy of it is left in the app`() {
        // ADR-0073 R.2 with rule 4: the production rule is `resetVerdict` on
        // the UniFFI surface. The hand-written table this file used to test
        // row by row is gone; what the gate does with each outcome is
        // `ResetGateTest` and `StartedOverGateTest`.
        val root = listOf("src/main/kotlin", "app/src/main/kotlin").map { java.io.File(it) }.first { it.exists() }
        val rule = java.io.File(root, "me/lakko/app/contacts/ResetVerdict.kt").readText()
        assertTrue(rule.contains("uniffi.lakko_ffi_uniffi.resetVerdict("))
        assertFalse("no hand-written rule function", rule.contains("fun resetVerdict("))
        assertFalse(rule.contains("logOffAcceptsReset"))
        val client = java.io.File(root, "me/lakko/app/contacts/ContactsClient.kt").readText()
        assertTrue(client.contains("private val resetRule: ResetRule = NativeResetRule"))
    }

    @Test
    fun `the outcomes are sealed, unsealed, refused and no verdict`() {
        val all: List<ResetVerdict> = listOf(
            ResetVerdict.Sealed(2, 1_000, 5),
            ResetVerdict.Unsealed(2, 1_000),
            ResetVerdict.Refused(ResetRefusal.UNCONFIRMED),
            ResetVerdict.NoVerdict,
        )
        assertEquals(4, all.map { it::class }.toSet().size)
        // No seal could be checked: the rule reads that as no valid seal.
        assertFalse(SealCheck.NONE.valid)
        assertEquals(0L, SealCheck.NONE.announcementSeq)
    }

    @Test
    fun `the re-pin marker never moves backwards`() {
        assertEquals(5_000L, resetRepinMarker(5_000, 1_000_000))
        assertEquals(1_000L, resetRepinMarker(0, 1_000_999))
    }

    // ---- the verified binding ------------------------------------------

    private val binding = VerifiedBinding("roster", 1, 10)

    @Test
    fun `memberHeldKey - the six rows`() {
        assertEquals(MemberHeldVerdict.Pin, memberHeldKey(true, null, "roster", "other"))
        assertEquals(MemberHeldVerdict.Record, memberHeldKey(false, null, "roster", "roster"))
        // The attack: a claimed account id whose directory key is another's.
        assertEquals(MemberHeldVerdict.None, memberHeldKey(false, null, "roster", "victim"))
        assertEquals(MemberHeldVerdict.Confirmed, memberHeldKey(false, binding, "roster", "roster"))
        assertEquals(MemberHeldVerdict.Held("roster", 1), memberHeldKey(false, binding, "roster", "new"))
        assertEquals(MemberHeldVerdict.None, memberHeldKey(false, binding, "claimed", "roster"))
        assertEquals(MemberHeldVerdict.None, memberHeldKey(false, binding, "claimed", "claimed"))
    }

    @Test
    fun `a binding is write-once and a junk row reads as none`() {
        val store = InMemoryVerifiedBindingStore()
        assertTrue(store.record("me", "acct", binding))
        assertFalse(store.record("me", "acct", VerifiedBinding("other", 5, 20)))
        assertEquals(binding, store.read("me", "acct"))
        assertNull(store.read("someone-else", "acct"))
        store.drop("me", "acct")
        assertNull(store.read("me", "acct"))
        assertFalse(store.record("me", "acct", VerifiedBinding("", 1, 1)))
        assertEquals(mapOf("a" to binding), decodeVerifiedBindings(encodeVerifiedBindings(mapOf("a" to binding))))
        assertTrue(decodeVerifiedBindings("{not json").isEmpty())
        assertTrue(decodeVerifiedBindings("""{"a":{"keyB64":"","keyGeneration":1,"verifiedAtMs":1}}""").isEmpty())
    }

    // ---- the re-pin plan -----------------------------------------------

    private fun reset(generation: Long = 2, newKey: String = "new", held: String = "old") =
        AcceptedReset("acct", held, newKey, generation, signedAtMs = 500, nowMs = 9_000_000)

    @Test
    fun `an accepted reset re-pins and retires the held key and its proven predecessors`() {
        val plan = acceptResetPlan(
            pin = Pin("old", rotationMarker = 100),
            row = ResetLedgerRow(acceptedKeyGeneration = 1, previousKeysB64 = listOf("ancient")),
            provenPredecessorsB64 = listOf("older"),
            reset = reset(),
        ) as AcceptResetPlan.Accepted
        assertEquals(9_000L, plan.marker)
        assertEquals(listOf("older", "old"), plan.retiredKeysB64)
        assertEquals(2L, plan.row.acceptedKeyGeneration)
        assertEquals(listOf("ancient", "older", "old"), plan.row.previousKeysB64)
        assertEquals(listOf("older", "old"), plan.row.supersededKeysB64)
        assertEquals(500L, plan.row.resetSignedAtMs)
    }

    @Test
    fun `the store repeats the generation check`() {
        val row = ResetLedgerRow(acceptedKeyGeneration = 2)
        assertEquals(AcceptResetPlan.Stale, acceptResetPlan(Pin("old", 0), row, emptyList(), reset()))
        assertEquals(AcceptResetPlan.Stale, acceptResetPlan(Pin("old", 0), row, emptyList(), reset(generation = 1)))
        assertEquals(AcceptResetPlan.Already, acceptResetPlan(Pin("new", 0), row, emptyList(), reset()))
        // A marker ahead of the clock is kept.
        val plan = acceptResetPlan(Pin("old", 99_999_999), null, emptyList(), reset()) as AcceptResetPlan.Accepted
        assertEquals(99_999_999L, plan.marker)
    }

    @Test
    fun `a member never pinned is accepted from its binding's key`() {
        val plan = acceptResetPlan(null, null, emptyList(), reset(held = "roster")) as AcceptResetPlan.Accepted
        assertEquals(listOf("roster"), plan.retiredKeysB64)
        assertEquals(listOf("roster"), plan.row.supersededKeysB64)
    }

    @Test
    fun `the generation baseline is written once`() {
        assertEquals(ResetLedgerRow(acceptedKeyGeneration = 3), withGenerationBaseline(null, 3))
        assertNull(withGenerationBaseline(ResetLedgerRow(acceptedKeyGeneration = 1), 3))
        assertNull(withGenerationBaseline(null, -1))
    }

    @Test
    fun `superseded keys, aliases and the accepted reset of a key`() {
        val rows = mapOf(
            "a" to ResetLedgerRow(2, listOf("a-old", "a-older"), listOf("a-old"), 500),
            "b" to ResetLedgerRow(1),
        )
        val pins = mapOf("a" to "a-new", "b" to "b-key")
        assertEquals(mapOf("a-old" to "a"), supersededKeysOf(rows))
        assertEquals(mapOf("a-old" to "a-new", "a-older" to "a-new"), retiredKeyAliases(rows) { pins[it] })
        assertEquals(AcceptedResetOfKey("a", 2, 500), acceptedResetOfKey("a-new", rows) { pins[it] })
        // A first pin (no reset) is reached with the grants the person issued.
        assertNull(acceptedResetOfKey("b-key", rows) { pins[it] })
        assertNull(acceptedResetOfKey("a-old", rows) { pins[it] })
        assertEquals(rows, decodeResetLedger(encodeResetLedger(rows)))
        assertTrue(decodeResetLedger("nope").isEmpty())
    }

    @Test
    fun `the contact row follows the person and a second row folds in`() {
        val rows = listOf(
            ContactRowKeys("x", "x-key"),
            ContactRowKeys("acct", "old"),
            ContactRowKeys(null, "new"),
        )
        assertEquals(ContactRekeyPlan(1, setOf(2)), rekeyContactRows(rows, "acct", listOf("old"), "new"))
        // Found by a retired key when the row carries no account id.
        assertEquals(
            ContactRekeyPlan(0, emptySet()),
            rekeyContactRows(listOf(ContactRowKeys(null, "old")), "acct", listOf("old"), "new"),
        )
        assertNull(rekeyContactRows(listOf(ContactRowKeys("acct", "new")), "acct", listOf("old"), "new"))
        assertNull(rekeyContactRows(listOf(ContactRowKeys("other", "k")), "acct", listOf("old"), "new"))
    }

    // ---- requests -------------------------------------------------------

    @Test
    fun `contact request gate and target`() {
        assertEquals(ContactRequestGateVerdict.SEND, contactRequestGate(null, "k"))
        assertEquals(ContactRequestGateVerdict.SEND, contactRequestGate("k", "k"))
        assertEquals(ContactRequestGateVerdict.CHECK_IDENTITY, contactRequestGate("pin", "k"))
        assertEquals(RequestTarget.AS_SUPPLIED, requestTargetAfterFetch("k", "k", false))
        assertEquals(RequestTarget.CURRENT, requestTargetAfterFetch("old", "new", true))
        assertEquals(RequestTarget.REFUSE, requestTargetAfterFetch("old", "new", false))
    }

    @Test
    fun `a request from a known contact is answered, dropped or kept waiting, never shown`() {
        fun v(contact: Boolean, check: KnownRequestCheck, trusted: String? = "k") =
            knownContactRequestVerdict(contact, check, "k", trusted)
        assertEquals(KnownRequestVerdict.SHOW, v(false, KnownRequestCheck.REFUSED))
        assertEquals(KnownRequestVerdict.DROP, v(true, KnownRequestCheck.REFUSED))
        assertEquals(KnownRequestVerdict.WAIT, v(true, KnownRequestCheck.INCONCLUSIVE))
        assertEquals(KnownRequestVerdict.ANSWER, v(true, KnownRequestCheck.CURRENT))
        assertEquals(KnownRequestVerdict.DROP, v(true, KnownRequestCheck.CURRENT, trusted = "other"))
        assertEquals(KnownRequestVerdict.DROP, v(true, KnownRequestCheck.CURRENT, trusted = null))
    }

    @Test
    fun `a contact is established only by a key this device verified for the account`() {
        val rows = listOf(ContactRowKeys("acct", "claimed"), ContactRowKeys("acct", "real"), ContactRowKeys("x", "real2"))
        assertNull(establishedContactOfAccount(rows, "nobody", listOf("real"), null))
        assertEquals(1, establishedContactOfAccount(rows, "acct", listOf("real", "pred"), null))
        assertEquals(1, establishedContactOfAccount(rows, "acct", emptyList(), "real"))
        // A row completed only from a profile: an ordinary request row.
        assertNull(establishedContactOfAccount(rows.take(1), "acct", listOf("real"), null))
        assertNull(establishedContactOfAccount(rows, "acct", emptyList(), null))
        assertNull(establishedContactOfAccount(rows, null, listOf("real"), null))
    }

    @Test
    fun `a row under the key that signed is that contact whatever it says of the account`() {
        assertTrue(isContactKey("signer", listOf("other", "signer")))
        val rows = listOf(ContactRowKeys(null, "signer"))
        assertNull(establishedContactOfAccount(rows, "acct", emptyList(), null))
        assertTrue(isContactKey("signer", rows.map { it.keyB64 }))
    }

    @Test
    fun `another key finds nothing so a claimed account id swallows no request`() {
        assertFalse(isContactKey("victim-key", listOf("member-key")))
        assertFalse(isContactKey("", listOf("", "member-key")))
    }

    @Test
    fun `devices bound to another key than the expected one are never added`() {
        assertEquals(setOf("k"), acceptableBoundKeys("k", null))
        assertEquals(setOf("k"), acceptableBoundKeys("k", listOf("pinned", "pred")))
        assertEquals(setOf("pred", "pinned"), acceptableBoundKeys("pred", listOf("pinned", "pred")))
        assertTrue(acceptableBoundKeys("", listOf("pinned")).isEmpty())
        val bound = mapOf("d1" to "k", "d2" to "retired", "d3" to null)
        assertEquals(listOf("d1"), devicesBoundTo(listOf("d1", "d2", "d3"), setOf("k")) { bound[it] })
        assertTrue(devicesBoundTo(listOf("d1"), emptySet()) { bound[it] }.isEmpty())
    }
}
