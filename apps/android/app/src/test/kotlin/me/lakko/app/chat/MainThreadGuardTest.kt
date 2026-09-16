// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.Test

/**
 * The debug-only main-thread tripwire on the repository's MLS regions. The
 * default detector needs a `Looper`, so these tests inject one; the unit-test
 * variant is `debug`, which is what arms the guard at all.
 */
class MainThreadGuardTest {

    @After
    fun resetDetector() {
        MainThreadGuard.isMainThread = { false }
    }

    @Test
    fun `fires on the main thread in a debug build`() {
        MainThreadGuard.isMainThread = { true }
        try {
            MainThreadGuard.assertOffMain("pollGroup apply")
            fail("expected the guard to fire")
        } catch (error: IllegalStateException) {
            assertEquals("pollGroup apply must not run on the main thread", error.message)
        }
    }

    @Test
    fun `is silent off the main thread`() {
        MainThreadGuard.isMainThread = { false }
        MainThreadGuard.assertOffMain("pollGroup apply")
    }

    @Test
    fun `a detector that cannot answer never fails the caller`() {
        // The plain android.jar Looper stub throws off-device; the guard must
        // treat "cannot tell" as "nothing to check", not as a failure.
        MainThreadGuard.isMainThread = { throw RuntimeException("Method getMainLooper not mocked") }
        MainThreadGuard.assertOffMain("pollGroup apply")
    }
}
