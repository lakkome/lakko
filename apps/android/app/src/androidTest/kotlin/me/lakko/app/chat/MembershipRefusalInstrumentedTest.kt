// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.FfiRefusalDisposition
import uniffi.lakko_ffi_uniffi.FfiRefusalReason
import uniffi.lakko_ffi_uniffi.membershipRefusalDisposition
import uniffi.lakko_ffi_uniffi.membershipRefusalRetryMaySucceed

/**
 * ADR-0045 UX §6 / §12 item 6 — the REAL core's answer to *will the identical
 * call work later*.
 *
 * These call `liblakko_ffi_uniffi.so` (`membershipRefusalRetryMaySucceed` /
 * `membershipRefusalDisposition`), so they run inside the emulator / device
 * process rather than the host JVM — the same host-vs-instrumented split
 * [me.lakko.app.push.GroupPushRoutingTest] /
 * `GroupPushRoutingInstrumentedTest` already use.
 *
 * **What this half exists to catch, and what it cannot.** The host suite
 * [MembershipRefusalTest] drives the module through a port that STATES the
 * maintainer's rule, so it proves the module *asks* rather than deciding — a
 * module with a hard-coded partition fails it. It cannot prove the stated rule
 * is still the core's. That is what this file does, and it is the half that
 * would notice the core moving a reason between dispositions.
 *
 * Symmetrically, this file could not notice a module that hard-coded the
 * partition: it would agree with the core on every input.
 */
@RunWith(AndroidJUnit4::class)
class MembershipRefusalInstrumentedTest {
    init {
        // ADR-0061: the refusal is a catalog message; render it through the
        // real core. The English assertions below assume an English device,
        // as they always did.
        me.lakko.app.l10n.Localization.init(
            androidx.test.platform.app.InstrumentationRegistry.getInstrumentation().targetContext,
        )
    }

    /** The real core port — what `ChatViewModel` passes in production. */
    private val real: (FfiRefusalReason) -> Boolean = ::membershipRefusalRetryMaySucceed

    @Test
    fun theCoreCallsExactlyTheTwoFullQueuesRetryable() {
        // The maintainer's rule, asserted against the CORE rather than against
        // a restatement of it: retry when a request was refused because a
        // bounded queue was full, never when it was refused because of its own
        // state. `ROLE_CHANGE_PENDING` is a queue of capacity one (RFC 9420
        // admits at most one GroupContextExtensions proposal per commit).
        val retryable = FfiRefusalReason.entries.filter(real)
        assertEquals(
            listOf(FfiRefusalReason.ROLE_CHANGE_PENDING, FfiRefusalReason.PENDING_SET_FULL),
            retryable,
        )
        // The same set the host suite states. If the core ever moves a reason,
        // this fails here and the host suite's `expectedRetryable` has to move
        // with it.
        assertEquals(
            setOf(FfiRefusalReason.ROLE_CHANGE_PENDING, FfiRefusalReason.PENDING_SET_FULL),
            retryable.toSet(),
        )
    }

    @Test
    fun retryabilityIsNotTheComplementOfPermitted() {
        // The distinction the three-valued disposition exists for, and the one
        // a client inferring retryability from a name would get wrong: most
        // NOT-permitted refusals are not retryable, and neither are most of
        // the ones that are not permission problems at all.
        for (reason in FfiRefusalReason.entries) {
            val disposition = membershipRefusalDisposition(reason)
            assertEquals(
                "$reason",
                disposition == FfiRefusalDisposition.RETRY_LATER,
                real(reason),
            )
        }
        // DO_NOT_RETRY is populated — otherwise the assertion above would be
        // the two-valued shape the core argues is wrong, and would pass.
        assertTrue(
            FfiRefusalReason.entries.any {
                membershipRefusalDisposition(it) == FfiRefusalDisposition.DO_NOT_RETRY
            },
        )
    }

    @Test
    fun theModuleRendersTheCoresAnswerForEveryReason() {
        // The production wiring end to end: real core port in, sentence out.
        for (reason in FfiRefusalReason.entries) {
            val refusal = membershipRefusal(reason, real)
            assertEquals("$reason", real(reason), refusal.retryMaySucceed)
            assertTrue("$reason rendered an empty sentence", refusal.message.render().isNotEmpty())
            // The invitation to try again appears on exactly the retryable
            // ones — which is the user-visible consequence of the whole split.
            assertEquals("$reason", real(reason), refusal.message.render().contains("try again"))
        }
    }

    @Test
    fun theSpecRowsReadTheSameUnderTheRealCore() {
        // §6's approved copy, rendered through the real port rather than a
        // stated rule — the row whose sentence DEPENDS on the core's answer is
        // row 6, and this is where that dependency is actually exercised.
        assertEquals(
            "A role change is already waiting to apply. Wait for it to land, then try again.",
            membershipRefusal(FfiRefusalReason.ROLE_CHANGE_PENDING, real).message.render(),
        )
        assertEquals(
            "Too many changes are already waiting to apply. Wait for them to land, then try again.",
            membershipRefusal(FfiRefusalReason.PENDING_SET_FULL, real).message.render(),
        )
        assertEquals(
            "The group owner cannot be removed.",
            membershipRefusal(FfiRefusalReason.OWNER_UNREMOVABLE, real).message.render(),
        )
        assertFalse(
            membershipRefusal(FfiRefusalReason.OWNER_UNREMOVABLE, real).message.render()
                .contains("try again"),
        )
    }
}
