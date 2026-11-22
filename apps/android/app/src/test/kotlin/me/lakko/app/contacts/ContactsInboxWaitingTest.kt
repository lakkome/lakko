// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0073 R.7 case 1 / UX 16.6 - the unshown requests from existing
 *  contacts that wait for a verdict (mirror of the web `inbox-store` waiting
 *  cases). */
class ContactsInboxWaitingTest {
    private fun request(messageId: Long) = IncomingContactRequest(
        messageId = messageId,
        fromPubkey = ByteArray(32) { messageId.toByte() },
        fromAccountId = ByteArray(16) { 7 },
        fromPseudonym = ByteArray(32) { (messageId + 1).toByte() },
        validAfterMs = 1_700_000_000_000uL,
        intro = null,
        createdAt = "2026-09-16T10:00:00Z",
    )

    @Test
    fun `a request is held once by its message id`() {
        val first = withWaitingRequest(emptyList(), request(1))
        assertTrue(first.kept)
        assertEquals(listOf(1L), first.waiting.map { it.messageId })
        val again = withWaitingRequest(first.waiting, request(1))
        assertTrue(again.kept)
        assertSame(first.waiting, again.waiting)
        assertEquals(listOf(1L, 2L), withWaitingRequest(first.waiting, request(2)).waiting.map { it.messageId })
    }

    @Test
    fun `a full list refuses one more and keeps what it holds`() {
        val full = (1L..MAX_WAITING_REQUESTS).map(::request)
        val next = withWaitingRequest(full, request(999))
        assertFalse(next.kept)
        assertSame(full, next.waiting)
        // One already held is still "kept" when the list is full.
        assertTrue(withWaitingRequest(full, request(3)).kept)
    }

    @Test
    fun `the store keeps the waiting requests apart from the pending ones`() {
        val store = InMemoryContactsInboxStore()
        store.savePending("acct", listOf(request(1)))
        store.saveWaiting("acct", listOf(request(2), request(3)))
        val snapshot = store.load("acct")
        assertEquals(listOf(1L), snapshot.pending.map { it.messageId })
        assertEquals(listOf(2L, 3L), snapshot.waiting.map { it.messageId })
        assertTrue(store.load("other").waiting.isEmpty())
        store.saveWaiting("acct", emptyList())
        assertTrue(store.load("acct").waiting.isEmpty())
        assertEquals(listOf(1L), store.load("acct").pending.map { it.messageId })
        store.saveWaiting("acct", listOf(request(4)))
        store.delete("acct")
        assertTrue(store.load("acct").waiting.isEmpty())
    }

    @Test
    fun `the verdict for a request from an existing contact`() {
        val key = "KEY"
        assertEquals(KnownRequestVerdict.SHOW, knownContactRequestVerdict(false, KnownRequestCheck.CURRENT, key, key))
        assertEquals(KnownRequestVerdict.DROP, knownContactRequestVerdict(true, KnownRequestCheck.REFUSED, key, key))
        assertEquals(KnownRequestVerdict.WAIT, knownContactRequestVerdict(true, KnownRequestCheck.INCONCLUSIVE, key, key))
        assertEquals(KnownRequestVerdict.ANSWER, knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, key))
        assertEquals(KnownRequestVerdict.DROP, knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, "OTHER"))
        assertEquals(KnownRequestVerdict.DROP, knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, null))
    }

    @Test
    fun `a sibling may still admit this device - the request waits, then is listed`() {
        val key = "KEY"
        assertEquals(
            KnownRequestVerdict.WAIT,
            knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, key, holdsOneToOne = false, siblingMayAdmit = true),
        )
        assertEquals(
            KnownRequestVerdict.ANSWER,
            knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, key, holdsOneToOne = true, siblingMayAdmit = true),
        )
        assertEquals(
            KnownRequestVerdict.DROP,
            knownContactRequestVerdict(
                true, KnownRequestCheck.CURRENT, key, key,
                holdsOneToOne = false, predatesContact = true, siblingMayAdmit = true,
            ),
        )
        val at = "2026-11-22T09:00:00Z"
        val t = java.time.Instant.parse(at).toEpochMilli()
        assertTrue(siblingMayAdmit(1, at, t + 1000))
        assertFalse(siblingMayAdmit(1, at, t + SIBLING_ADMIT_WAIT_MS))
        assertFalse(siblingMayAdmit(0, at, t + 1000))
        assertFalse(siblingMayAdmit(null, at, t + 1000))
        assertFalse(siblingMayAdmit(2, "bad", t))
    }

    @Test
    fun `after a lost cursor a request no newer than the contact is dropped, not listed`() {
        val key = "KEY"
        assertEquals(
            KnownRequestVerdict.DROP,
            knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, key, holdsOneToOne = false, predatesContact = true),
        )
        assertEquals(
            KnownRequestVerdict.ANSWER,
            knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, key, holdsOneToOne = true, predatesContact = true),
        )
        val since = "2026-11-22T09:00:00Z"
        assertTrue(requestPredatesContact("2026-11-21T09:00:00Z", since))
        assertTrue(requestPredatesContact("2026-11-22T09:00:00Z", since))
        assertFalse(requestPredatesContact("2026-11-22T09:19:00Z", since))
        assertFalse(requestPredatesContact("not a date", since))
        assertFalse(requestPredatesContact("2026-11-21T09:00:00Z", null))
        assertFalse(requestPredatesContact("2026-11-21T09:00:00Z", ""))
    }

    @Test
    fun `base scenario 7 - answered silently only while a one-to-one is held, else listed`() {
        val key = "KEY"
        assertEquals(
            KnownRequestVerdict.ANSWER,
            knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, key, holdsOneToOne = true),
        )
        assertEquals(
            KnownRequestVerdict.SHOW,
            knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, key, holdsOneToOne = false),
        )
        // Dropping and waiting do not depend on it.
        assertEquals(
            KnownRequestVerdict.DROP,
            knownContactRequestVerdict(true, KnownRequestCheck.CURRENT, key, "OTHER", holdsOneToOne = false),
        )
        assertEquals(
            KnownRequestVerdict.WAIT,
            knownContactRequestVerdict(true, KnownRequestCheck.INCONCLUSIVE, key, key, holdsOneToOne = false),
        )
    }
}
