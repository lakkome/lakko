// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

/** The log line of a Welcome from the account mailbox names what became of it. */
class WelcomeOutcomeTest {

    @Test
    fun `a Welcome sealed to another device is told apart from a refusal`() {
        val other = RuntimeException("joinFromWelcome failed", RuntimeException("Welcome(NoMatchingKeyPackage)"))
        assertEquals(WelcomeOutcome.FOR_ANOTHER_DEVICE, welcomeOutcomeOf(other))
        val active = RuntimeException(
            "joinFromWelcome failed",
            uniffi.lakko_ffi_uniffi.FfiException.MembershipGroupStillActive("ab".repeat(32)),
        )
        assertEquals(WelcomeOutcome.HELD_GROUP_STILL_ACTIVE, welcomeOutcomeOf(active))
        assertEquals(WelcomeOutcome.REFUSED, welcomeOutcomeOf(IllegalStateException("NoRoleTable")))
    }
}
