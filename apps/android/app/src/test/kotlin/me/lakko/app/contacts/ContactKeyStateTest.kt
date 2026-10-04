// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.ui.contacts.contactRouteKey
import me.lakko.app.ui.contacts.refusalMessage
import me.lakko.app.ui.contacts.refusalOpenContactRouteKey
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0065 phase 2 / UX 16.1, 16.2, 16.8 — the contact's reset state.
 *
 *  1. The trigger: only a refusal for an ABSENT proof that the log confirmed
 *     as a reset becomes the state; everything else keeps the error copy.
 *  2. The confirmation re-pins with a marker that never moves backwards.
 *  3. Re-observations merge: the same pending key keeps the record, a later
 *     reset moves to the new key and keeps the first previous key.
 *  4. The store codec round-trips and reads garbage as absent.
 *  5. The in-memory store scopes records per owner account.
 */
class ContactKeyStateTest {

    // --- 1. the trigger ----------------------------------------------------

    @Test
    fun `only an absent proof with a logged reset becomes the reset state`() {
        assertTrue(refusalIsLoggedReset(SupersessionRefusedReason.ProofAbsent, loggedReset = true))
        assertFalse(refusalIsLoggedReset(SupersessionRefusedReason.ProofAbsent, loggedReset = false))
        assertFalse(refusalIsLoggedReset(SupersessionRefusedReason.ProofAbsent, loggedReset = null))
        assertFalse(refusalIsLoggedReset(SupersessionRefusedReason.ProofInvalid, loggedReset = true))
        assertFalse(refusalIsLoggedReset(SupersessionRefusedReason.KeyMismatch, loggedReset = true))
        assertFalse(refusalIsLoggedReset(SupersessionRefusedReason.MarkerBackward, loggedReset = true))
    }

    @Test
    fun `only a logged reset offers Open contact, under the pinned key`() {
        val pinned = java.util.Base64.getEncoder().encodeToString(ByteArray(32) { 7 })
        assertEquals(
            pinned,
            refusalOpenContactKeyB64(SupersessionRefusedReason.ProofAbsent, loggedReset = true, pinnedPubkeyB64 = pinned),
        )
        // A possible compromise has no contact state to open.
        assertNull(refusalOpenContactKeyB64(SupersessionRefusedReason.ProofAbsent, loggedReset = false, pinnedPubkeyB64 = pinned))
        assertNull(refusalOpenContactKeyB64(SupersessionRefusedReason.ProofAbsent, loggedReset = null, pinnedPubkeyB64 = pinned))
        assertNull(refusalOpenContactKeyB64(SupersessionRefusedReason.KeyMismatch, loggedReset = true, pinnedPubkeyB64 = pinned))
        // No pin: nothing to open.
        assertNull(refusalOpenContactKeyB64(SupersessionRefusedReason.ProofAbsent, loggedReset = true, pinnedPubkeyB64 = null))
        assertNull(refusalOpenContactKeyB64(SupersessionRefusedReason.ProofAbsent, loggedReset = true, pinnedPubkeyB64 = ""))

        val reset = SupersessionRefused(SupersessionRefusedReason.ProofAbsent, "acct", "m").apply {
            loggedReset = true
            pinnedPubkeyB64 = pinned
        }
        assertEquals(contactRouteKey(ByteArray(32) { 7 }), refusalOpenContactRouteKey(reset))
        val undecodable = SupersessionRefused(SupersessionRefusedReason.ProofAbsent, "acct", "m").apply {
            loggedReset = true
            pinnedPubkeyB64 = "not base64 !!"
        }
        assertNull(refusalOpenContactRouteKey(undecodable))
        val unexplained = SupersessionRefused(SupersessionRefusedReason.ProofAbsent, "acct", "m").apply {
            loggedReset = false
            pinnedPubkeyB64 = pinned
        }
        assertNull(refusalOpenContactRouteKey(unexplained))
    }

    @Test
    fun `a logged reset points the action at the state and every other refusal keeps the error copy`() {
        val reset = SupersessionRefused(SupersessionRefusedReason.ProofAbsent, "acct", "m").apply { loggedReset = true }
        assertEquals("contacts.keys_reset.verify_first", refusalMessage(reset, "@anna").key)
        assertEquals("@anna", refusalMessage(reset, "@anna").texts["name"])
        assertEquals("contacts.keys_reset.verify_first_short", refusalMessage(reset, null).key)

        val unexplained = SupersessionRefused(SupersessionRefusedReason.ProofAbsent, "acct", "m").apply { loggedReset = false }
        assertEquals("contacts.identity_changed.unexplained", refusalMessage(unexplained, "@anna").key)
        val unconfirmed = SupersessionRefused(SupersessionRefusedReason.ProofAbsent, "acct", "m")
        assertEquals("contacts.identity_changed.unconfirmed_unnamed", refusalMessage(unconfirmed, null).key)
    }

    // --- 2. the re-pin marker --------------------------------------------

    @Test
    fun `the re-pin marker never moves backwards and a later reset carries a later marker`() {
        assertEquals(1_000L, resetRepinMarker(currentMarker = 0L, nowSecs = 1_000L))
        // A proof-pinned marker in the future stays where it is.
        assertEquals(5_000L, resetRepinMarker(currentMarker = 5_000L, nowSecs = 1_000L))
        val first = resetRepinMarker(0L, 1_000L)
        val second = resetRepinMarker(first, 2_000L)
        assertTrue(second > first)
    }

    @Test
    fun `confirming keeps the previous key and flips the state`() {
        val record = resetRecordFor("acct", "prev", "next", 10L, 2L)
        assertTrue(record.unverified)
        val confirmed = confirmedRecord(record)
        assertFalse(confirmed.unverified)
        assertEquals("prev", confirmed.previousKeyB64)
        assertEquals("next", confirmed.pendingKeyB64)
        assertEquals(2L, confirmed.keyGeneration)
    }

    // --- 3. re-observations ------------------------------------------------

    @Test
    fun `a re-observation of the same pending key keeps the first record`() {
        val first = resetRecordFor("acct", "prev", "next", 10L, 2L)
        val again = resetRecordFor("acct", "prev", "next", 20L, 2L)
        assertSame(first, mergeResetObservation(first, again))
    }

    @Test
    fun `a later reset moves to the new key and keeps the first previous key while unverified`() {
        val first = resetRecordFor("acct", "prev", "next", 10L, 2L)
        val later = resetRecordFor("acct", "next", "next2", 20L, 3L)
        val merged = mergeResetObservation(first, later)
        assertEquals("next2", merged.pendingKeyB64)
        assertEquals("prev", merged.previousKeyB64)
        assertTrue(merged.unverified)
    }

    @Test
    fun `a reset after a verification treats the verified key as the previous one`() {
        val verified = confirmedRecord(resetRecordFor("acct", "prev", "next", 10L, 2L))
        val later = resetRecordFor("acct", "next", "next2", 20L, 3L)
        val merged = mergeResetObservation(verified, later)
        assertEquals("next", merged.previousKeyB64)
        assertEquals("next2", merged.pendingKeyB64)
        assertTrue(merged.unverified)
    }

    @Test
    fun `nothing stored takes the incoming record as is`() {
        val incoming = resetRecordFor("acct", "prev", "next", 10L, null)
        assertSame(incoming, mergeResetObservation(null, incoming))
    }

    // --- 4. codec ----------------------------------------------------------

    @Test
    fun `the store codec round-trips both states and reads garbage as absent`() {
        val flagged = resetRecordFor("acct", "prev", "next", 10L, 2L)
        assertEquals(flagged, decodeResetRecord(encodeResetRecord(flagged)))
        val confirmed = confirmedRecord(flagged.copy(keyGeneration = null))
        assertEquals(confirmed, decodeResetRecord(encodeResetRecord(confirmed)))
        assertNull(decodeResetRecord(null))
        assertNull(decodeResetRecord(""))
        assertNull(decodeResetRecord("not json"))
        assertNull(decodeResetRecord("""{"accountIdB64":"a","previousKeyB64":"p","pendingKeyB64":"n","observedAtMs":1,"state":"BOGUS"}"""))
        assertNull(decodeResetRecord("""{"accountIdB64":"","previousKeyB64":"p","pendingKeyB64":"n","observedAtMs":1,"state":"NORMAL"}"""))
    }

    // --- 5. the store -------------------------------------------------------

    @Test
    fun `the in-memory store scopes records per owner account`() {
        val store = InMemoryContactKeyStateStore()
        val record = resetRecordFor("peer", "prev", "next", 1L, 2L)
        store.save("ownerA", record)
        assertEquals(record, store.load("ownerA", "peer"))
        assertNull(store.load("ownerB", "peer"))
        assertEquals(listOf(record), store.loadAll("ownerA"))
        store.remove("ownerA", "peer")
        assertNull(store.load("ownerA", "peer"))
    }

    @Test
    fun `clearing one owner leaves the other owner's records`() {
        val store = InMemoryContactKeyStateStore()
        store.save("ownerA", resetRecordFor("peer", "prev", "next", 1L, 2L))
        store.save("ownerB", resetRecordFor("peer", "prev", "next", 1L, 2L))
        store.clearForOwner("ownerA")
        assertTrue(store.loadAll("ownerA").isEmpty())
        assertNotNull(store.load("ownerB", "peer"))
    }

    @Test
    fun `the copy keys exist for the named and the unnamed contact`() {
        assertEquals("contacts.keys_reset.banner", KeysResetCopy.banner("Anna").key)
        assertEquals("contacts.keys_reset.banner_unnamed", KeysResetCopy.banner(null).key)
        assertEquals("contacts.keys_reset.row_sub", KeysResetCopy.ROW_SUB.key)
        assertEquals("contacts.keys_reset.picker_sub", KeysResetCopy.PICKER_SUB.key)
    }

    // ---- UX 16.1: keys handed over rather than fetched ----------------------

    @Test
    fun `a stranger with no stored pin can be sent a request`() {
        assertEquals(
            BundleRecipientVerdict.SEND,
            bundleRecipientVerdict(resetUnverified = false, pinnedKeyB64 = null, recipientKeyB64 = "new"),
        )
    }

    @Test
    fun `the pinned key of an ordinary contact is sent to`() {
        assertEquals(
            BundleRecipientVerdict.SEND,
            bundleRecipientVerdict(resetUnverified = false, pinnedKeyB64 = "k", recipientKeyB64 = "k"),
        )
    }

    @Test
    fun `a flagged contact is refused whichever key is named`() {
        for (key in listOf("old", "new")) {
            assertEquals(
                BundleRecipientVerdict.REFUSE_RESET_UNVERIFIED,
                bundleRecipientVerdict(resetUnverified = true, pinnedKeyB64 = "old", recipientKeyB64 = key),
            )
        }
        // Even with no pin left to compare against.
        assertEquals(
            BundleRecipientVerdict.REFUSE_RESET_UNVERIFIED,
            bundleRecipientVerdict(resetUnverified = true, pinnedKeyB64 = null, recipientKeyB64 = "new"),
        )
    }

    @Test
    fun `a pinned account named under another key is refused before the reset is even noticed`() {
        assertEquals(
            BundleRecipientVerdict.REFUSE_KEY_CHANGED,
            bundleRecipientVerdict(resetUnverified = false, pinnedKeyB64 = "old", recipientKeyB64 = "new"),
        )
    }
}
