// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * Seen live 2026-11-13: a public channel's first metadata record did not
 * post, and its re-issue, composed with no record held, told every reader
 * the channel was private. The first record is owed until it lands, and a
 * device holding no record re-issues nothing rather than a record whose flag
 * it does not know.
 */
@RunWith(RobolectricTestRunner::class)
class ChannelFirstRecordTest {

    private val context: Context = ApplicationProvider.getApplicationContext()

    @Before
    fun clean() {
        PrefsOwedMarks.deleteAccount(context, "A")
    }

    @Test
    fun a_failed_first_record_stays_owed_and_a_landed_one_clears() = runBlocking {
        val marks = PrefsOwedRecordMarks(context) { "A" }
        assertFalse(postFirstChannelRecord(marks, "chan") { throw IllegalStateException("offline") })
        assertTrue("a later pass re-issues it", PrefsOwedRecordMarks(context) { "A" }.isOwed("chan"))
        assertTrue(postFirstChannelRecord(marks, "chan") { })
        assertFalse(PrefsOwedRecordMarks(context) { "A" }.isOwed("chan"))
    }

    @Test
    fun a_reissue_with_no_record_held_sends_nothing() = runBlocking {
        var requests = 0
        val client = ChannelsClient(
            baseUrl = "http://test.invalid",
            engine = MockEngine {
                requests += 1
                respond(content = "", status = HttpStatusCode.InternalServerError)
            },
        )
        val state = ChannelsUiState()
        state.addChannel(
            ChannelHandle(
                id = "0123456789abcdef0123456789abcdef",
                localName = "Walk 28 channel",
                localDescription = "",
                addedAtMs = 1uL,
                handle = "walk_28_channel",
            ),
        )
        client.use {
            // Nothing to issue: settled, and no record defaulting to "not
            // public" leaves this device.
            assertTrue(ChannelMetaPublisher.reissue(state, it, "0123456789abcdef0123456789abcdef"))
        }
        assertEquals(0, requests)
    }
}
