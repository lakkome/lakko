// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** A call's media is closed off the calling thread, in order, and one failure
 *  does not stop the next close. */
class CallMediaCloserTest {

    @Test
    fun `closes run in order on their own thread`() {
        val order = java.util.Collections.synchronizedList(ArrayList<String>())
        var thread: Thread? = null
        val done = CountDownLatch(1)
        CallMediaCloser.close {
            thread = Thread.currentThread()
            order += "first"
        }
        CallMediaCloser.close { throw IllegalStateException("peer gone") }
        CallMediaCloser.close {
            order += "second"
            done.countDown()
        }
        assertTrue(done.await(5, TimeUnit.SECONDS))
        assertEquals(listOf("first", "second"), order)
        assertNotEquals(Thread.currentThread(), thread)
    }
}
