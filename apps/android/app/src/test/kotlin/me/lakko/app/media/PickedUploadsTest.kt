// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import me.lakko.app.chat.AttachmentBridgeException
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** UX spec §10 — a picked item never disappears silently. */
class PickedUploadsTest {
    init {
        EnglishForTests.install()
    }


    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    @After
    fun tearDown() {
        scope.cancel()
    }

    @Test
    fun aHangingPreviewIsAbandonedAtTheTimeout() = runBlocking {
        val started = System.nanoTime()
        val never = CompletableDeferred<String>()
        val result = awaitBounded(scope, timeoutMs = 150) { never.await() }
        val elapsedMs = (System.nanoTime() - started) / 1_000_000
        assertNull(result)
        assertTrue("returned after $elapsedMs ms", elapsedMs < 2_000)
    }

    @Test
    fun aFinishedPreviewIsReturned() = runBlocking {
        assertEquals("thumb", awaitBounded(scope, timeoutMs = 5_000) { "thumb" })
    }

    @Test
    fun aThrowingPreviewBecomesNoPreviewNotAFailure() = runBlocking {
        assertNull(awaitBounded<String>(scope, timeoutMs = 5_000) { throw IllegalStateException("decoder") })
    }

    @Test
    fun failureMessagesNameTheItemAndTheReason() {
        assertEquals(
            "\"clip.mp4\" is 60 B — over the 50 B limit.",
            attachFailureMessage("clip.mp4", AttachmentBridgeException.TooLarge(60, 50)).en,
        )
        assertEquals(
            "Could not upload \"a.jpg\". Try again.",
            attachFailureMessage("a.jpg", AttachmentBridgeException.Upload(RuntimeException())).en,
        )
        assertEquals(
            "Could not attach the selected item. Try again.",
            attachFailureMessage(null, null).en,
        )
        assertEquals(
            "Could not attach the selected item. Try again.",
            attachFailureMessage(" ", IllegalStateException()).en,
        )
    }
}
