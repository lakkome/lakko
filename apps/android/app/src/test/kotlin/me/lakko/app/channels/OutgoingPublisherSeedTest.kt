// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * A publisher keeps the seed its keys are replaced from.
 *
 * Seen on a live walk: right after an approve, the owner's own new channel
 * (no posts, only its record at creation) read "Earlier posts aren't
 * available.", because the reader held the seed of the current epoch only.
 */
class OutgoingPublisherSeedTest {
    private val old = ByteArray(32) { 1 }
    private val new = ByteArray(32) { 2 }

    @Test
    fun `the replaced seed is kept under the epoch it served`() {
        val kept = outgoingPublisherSeed(old, 3u, new, emptyMap())
        assertEquals(3u, kept?.first)
        assertArrayEquals(old, kept?.second)
    }

    @Test
    fun `nothing is kept when the seed does not change`() {
        // The second refresh of an approve (afterRotation) re-derives the same keys.
        assertNull(outgoingPublisherSeed(new, 3u, new.copyOf(), emptyMap()))
    }

    @Test
    fun `an epoch already holding a seed is not overwritten`() {
        assertNull(outgoingPublisherSeed(old, 3u, new, mapOf(3u to ByteArray(32) { 9 })))
    }

    @Test
    fun `nothing is kept without previous keys or a known epoch`() {
        assertNull(outgoingPublisherSeed(null, 3u, new, emptyMap()))
        assertNull(outgoingPublisherSeed(old, null, new, emptyMap()))
    }
}
