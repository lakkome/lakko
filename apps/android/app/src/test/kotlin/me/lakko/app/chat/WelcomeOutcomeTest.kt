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

    @Test
    fun `a Welcome refused as not a conversation raises no banner`() {
        // Walk of 2026-11-14 — a PublisherGroup Welcome in the account mailbox,
        // refused by the conversation join with the typed NoRoleTable (C-86).
        val notConversation = ChatError(
            "joinFromWelcome failed",
            uniffi.lakko_ffi_uniffi.FfiException.MembershipNoRoleTable(
                "welcome refused: the group carries no role table, so it is not a conversation",
            ),
        )
        assertEquals(true, isWelcomeNotAConversation(notConversation))
        assertEquals(WelcomeOutcome.NOT_A_CONVERSATION, welcomeOutcomeOf(notConversation))
        assertEquals(false, welcomeFailureRaisesBanner(notConversation, awaitingCoPublishWelcome = false))
        // A real refusal of this device's conversation Welcome still does.
        val refused = ChatError("joinFromWelcome failed", IllegalStateException("bad welcome"))
        assertEquals(true, welcomeFailureRaisesBanner(refused, awaitingCoPublishWelcome = false))
        assertEquals(false, welcomeFailureRaisesBanner(refused, awaitingCoPublishWelcome = true))
        // Typed, never by message: a plain error with the same words is not it.
        assertEquals(false, isWelcomeNotAConversation(IllegalStateException("carries no role table")))
        val other = RuntimeException("joinFromWelcome failed", RuntimeException("Welcome(NoMatchingKeyPackage)"))
        assertEquals(false, welcomeFailureRaisesBanner(other, awaitingCoPublishWelcome = false))
    }
}

