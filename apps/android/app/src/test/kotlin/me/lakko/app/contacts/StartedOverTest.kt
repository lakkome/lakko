// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.chat.ComposerSlot
import me.lakko.app.chat.MembershipState
import me.lakko.app.chat.callButtonsHidden
import me.lakko.app.chat.composerState
import me.lakko.app.chat.startedOverNotice
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0101 section 11 / UX 16.11 - a contact started over (an unsealed
 * reset): what this device records, what the old one-to-one shows, and what
 * happens to a request from the new keys.
 */
class StartedOverTest {

    private val seen = StartedOver(oldKeyB64 = "old", newKeyB64 = "new", keyGeneration = 2, signedAtMs = 700)

    // ---- the ledger --------------------------------------------------------

    @Test
    fun `an unsealed reset is recorded beside the pin's row and changes nothing else in it`() {
        val row = ResetLedgerRow(acceptedKeyGeneration = 1, supersededKeysB64 = listOf("older"), resetSignedAtMs = 50)
        val next = withStartedOver(row, seen)!!
        assertEquals(seen, next.startedOver)
        assertEquals(listOf("old"), next.startedOverKeysB64)
        assertEquals(1L, next.acceptedKeyGeneration)
        assertEquals(listOf("older"), next.supersededKeysB64)
        assertEquals(50L, next.resetSignedAtMs)
        // Seen again: nothing to write.
        assertNull(withStartedOver(next, seen))
        // An account this device had no row for.
        assertEquals(seen, withStartedOver(null, seen)!!.startedOver)
    }

    @Test
    fun `a later unsealed reset replaces the record and an earlier one never does`() {
        val first = withStartedOver(null, seen)!!
        val later = seen.copy(newKeyB64 = "newer", keyGeneration = 3)
        val second = withStartedOver(first, later)!!
        assertEquals(later, second.startedOver)
        assertEquals(listOf("old"), second.startedOverKeysB64)
        assertNull(withStartedOver(second, seen))
    }

    @Test
    fun `accepting the new account's request adopts its key as a new contact, not as a reset`() {
        val row = withStartedOver(
            ResetLedgerRow(acceptedKeyGeneration = 1, resetSignedAtMs = 50, sealSeqFloor = 9),
            seen,
        )!!
        val (adopted, open) = adoptStartedOver(row)!!
        assertEquals(seen, open)
        assertNull(adopted.startedOver)
        assertEquals(2L, adopted.acceptedKeyGeneration)
        // The old key stays closed.
        assertEquals(listOf("old"), adopted.startedOverKeysB64)
        // No accepted reset stands for this account: nothing is returned to
        // the new keys, and its announcements are numbered from the start.
        assertNull(adopted.resetSignedAtMs)
        assertEquals(0L, adopted.sealSeqFloor)
        // Nothing open: nothing to adopt.
        assertNull(adoptStartedOver(adopted))
        assertNull(adoptStartedOver(null))
    }

    @Test
    fun `the closed keys are known across accounts`() {
        val rows = mapOf(
            "acctA" to withStartedOver(null, seen)!!,
            "acctB" to ResetLedgerRow(acceptedKeyGeneration = 4),
            "acctC" to adoptStartedOver(withStartedOver(null, seen.copy(oldKeyB64 = "c-old")))!!.first,
        )
        assertEquals(mapOf("old" to "acctA", "c-old" to "acctC"), startedOverKeysOf(rows))
        assertTrue(startedOverKeysOf(emptyMap()).isEmpty())
    }

    @Test
    fun `a sealed reset is never found through an adopted new account`() {
        // The returns of ADR-0101 are planned from accepted resets only.
        val adopted = adoptStartedOver(withStartedOver(ResetLedgerRow(acceptedKeyGeneration = 1), seen))!!.first
        val rows = mapOf("acct" to adopted)
        assertNull(acceptedResetOfKey("new", rows) { "new" })
        assertNull(acceptedResetOfKey("old", rows) { "new" })
    }

    // ---- a request from the new keys ---------------------------------------

    @Test
    fun `a request from exactly the new account's key is shown with the note, any other is dropped`() {
        assertEquals(
            KnownRequestVerdict.SHOW_STARTED_OVER,
            knownContactRequestVerdict(true, KnownRequestCheck.STARTED_OVER, "new", "old", startedOverKeyB64 = "new"),
        )
        // Never answered silently, whatever else is true of the contact.
        assertEquals(
            KnownRequestVerdict.SHOW_STARTED_OVER,
            knownContactRequestVerdict(
                true, KnownRequestCheck.STARTED_OVER, "new", "old",
                holdsOneToOne = true, predatesContact = true, siblingMayAdmit = true, startedOverKeyB64 = "new",
            ),
        )
        assertEquals(
            KnownRequestVerdict.DROP,
            knownContactRequestVerdict(true, KnownRequestCheck.STARTED_OVER, "other", "old", startedOverKeyB64 = "new"),
        )
        assertEquals(
            KnownRequestVerdict.DROP,
            knownContactRequestVerdict(true, KnownRequestCheck.STARTED_OVER, "new", "old", startedOverKeyB64 = null),
        )
        // The old key itself (a replayed request): not the new account's.
        assertEquals(
            KnownRequestVerdict.DROP,
            knownContactRequestVerdict(true, KnownRequestCheck.STARTED_OVER, "old", "old", startedOverKeyB64 = "new"),
        )
        // A stranger is an ordinary request row.
        assertEquals(
            KnownRequestVerdict.SHOW,
            knownContactRequestVerdict(false, KnownRequestCheck.STARTED_OVER, "new", null, startedOverKeyB64 = "new"),
        )
    }

    // ---- the old one-to-one ------------------------------------------------

    @Test
    fun `the old one-to-one shows one closed line in place of the composer`() {
        val slot = composerState(MembershipState.Active, "half a sentence", peerStartedOver = "@alice")
        val withdrawn = slot as ComposerSlot.Withdrawn
        assertEquals(UiText("contacts.started_over.closed", texts = mapOf("name" to "@alice")), withdrawn.notice)
        assertEquals(startedOverNotice("@alice"), withdrawn.notice)
        // No draft block: nothing sent there would arrive.
        assertNull(withdrawn.draft)
        assertNull(withdrawn.draftLabel)
        // Nobody started over: the composer.
        assertEquals(ComposerSlot.Composer, composerState(MembershipState.Active, "", peerStartedOver = null))
    }

    @Test
    fun `a deleted account outranks a started-over one`() {
        val slot = composerState(MembershipState.Active, "", peerDeleted = true, peerStartedOver = "@alice") as ComposerSlot.Withdrawn
        assertFalse(slot.notice == startedOverNotice("@alice"))
    }

    @Test
    fun `the call buttons of the old one-to-one are hidden`() {
        assertTrue(
            callButtonsHidden(
                callOccupied = false, membershipEnded = false, callControls = true,
                peerDeleted = false, peerStartedOver = true,
            ),
        )
        assertFalse(
            callButtonsHidden(
                callOccupied = false, membershipEnded = false, callControls = true,
                peerDeleted = false, peerStartedOver = false,
            ),
        )
    }
}
