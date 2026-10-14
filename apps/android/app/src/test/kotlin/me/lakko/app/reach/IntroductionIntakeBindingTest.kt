// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * ADR-0060 section 4: an introduction envelope from a contact is kept in the
 * background, for the account that was bound when it was handed over. The
 * account can be gone, and another bound, before that work runs or while it
 * waits; a contact's introductions are never kept for the next account.
 */
class IntroductionIntakeBindingTest {

    @Test
    fun `still bound - kept, then the relay re-runs`() = runTest {
        val steps = mutableListOf<String>()
        runIntakeFor(stillBound = { true }, keep = { steps += "keep" }, then = { steps += "then" })
        assertEquals(listOf("keep", "then"), steps)
    }

    @Test
    fun `the account left before the work ran - nothing is kept`() = runTest {
        val steps = mutableListOf<String>()
        runIntakeFor(stillBound = { false }, keep = { steps += "keep" }, then = { steps += "then" })
        assertEquals(emptyList<String>(), steps)
    }

    @Test
    fun `the account left while the envelope was being kept - the relay is not re-run for the next one`() =
        runTest {
            val steps = mutableListOf<String>()
            var bound = true
            runIntakeFor(
                stillBound = { bound },
                keep = {
                    steps += "keep"
                    bound = false
                },
                then = { steps += "then" },
            )
            assertEquals(listOf("keep"), steps)
        }
}
