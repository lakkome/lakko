// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.testing

import java.io.File
import kotlin.coroutines.AbstractCoroutineContextElement
import kotlin.coroutines.CoroutineContext
import kotlinx.coroutines.CoroutineExceptionHandler

/**
 * Test hygiene: every exception a coroutine could not deliver to anyone is
 * written, with its stack, to the file named by the system property
 * `lakko.test.leakLog` (set by the Gradle unit-test task).
 *
 * `runTest` reports such an exception as `UncaughtExceptionsBeforeTest` in
 * whichever test happens to start next, which names the victim and not the
 * source. This handler is loaded by kotlinx.coroutines through
 * `META-INF/services/kotlinx.coroutines.CoroutineExceptionHandler` and sees
 * the exception when it happens; it consumes nothing, so `runTest` still
 * fails as before. The Gradle task fails when the file is not empty, so a
 * leak that no later `runTest` happened to pick up is not silent either.
 */
public class LeakedCoroutineExceptionLog :
    AbstractCoroutineContextElement(CoroutineExceptionHandler),
    CoroutineExceptionHandler {

    override fun handleException(context: CoroutineContext, exception: Throwable) {
        val path = System.getProperty(PROPERTY) ?: return
        val entry = buildString {
            append("---- leaked coroutine exception on thread ")
            append(Thread.currentThread().name)
            append(" at ")
            append(java.time.Instant.now())
            append('\n')
            append("context: ").append(context).append('\n')
            append(exception.stackTraceToString())
            append('\n')
        }
        synchronized(LOCK) {
            try {
                File(path).appendText(entry)
            } catch (_: Throwable) {
                // The log is a diagnostic; the test run goes on without it.
            }
        }
    }

    public companion object {
        public const val PROPERTY: String = "lakko.test.leakLog"
        private val LOCK = Any()
    }
}
