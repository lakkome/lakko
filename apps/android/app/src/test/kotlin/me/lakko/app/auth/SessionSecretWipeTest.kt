// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.StaleBindingException
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

/**
 * The session's seeds are zeroed in place at sign-out while work in flight
 * still holds the same arrays. That work reads a seed whole or is told the
 * session ended; it never builds a key from zeroes.
 */
class SessionSecretWipeTest {

    private fun seed() = ByteArray(64) { (it + 1).toByte() }

    @Test
    fun `a live seed is read as it is`() {
        val seed = seed()
        val read = SessionSecretWipe.use(seed) { it.copyOf() }
        assertArrayEquals(seed(), read)
    }

    @Test
    fun `a wiped seed is not read - the session has ended`() {
        val seed = seed()
        SessionSecretWipe.wipe(seed)
        var built = false
        try {
            SessionSecretWipe.use(seed) { built = true }
            fail("an identity was built from a wiped seed")
        } catch (_: StaleBindingException) {
            // expected
        }
        assertFalse(built)
    }

    @Test
    fun `the wipe zeroes every array it is given`() {
        val exportKey = ByteArray(64) { 9 }
        val seed = seed()
        SessionSecretWipe.wipe(exportKey, seed)
        assertTrue(exportKey.all { it == 0.toByte() })
        assertTrue(seed.all { it == 0.toByte() })
    }

    @Test
    fun `a wipe that lands during a read waits for it`() {
        val seed = seed()
        val reading = CountDownLatch(1)
        val release = CountDownLatch(1)
        var seenByReader: ByteArray? = null
        val reader = thread {
            SessionSecretWipe.use(seed) { bytes ->
                reading.countDown()
                release.await(5, TimeUnit.SECONDS)
                // What the core copies out: still whole, however long the
                // sign-out has been waiting.
                seenByReader = bytes.copyOf()
            }
        }
        assertTrue(reading.await(5, TimeUnit.SECONDS))
        val wiped = CountDownLatch(1)
        val wiper = thread {
            SessionSecretWipe.wipe(seed)
            wiped.countDown()
        }
        // The wipe is held off while the read is in progress.
        assertFalse(wiped.await(200, TimeUnit.MILLISECONDS))
        release.countDown()
        reader.join(5_000)
        wiper.join(5_000)

        assertArrayEquals(seed(), seenByReader)
        assertEquals(0, seed.count { it != 0.toByte() })
    }
}
