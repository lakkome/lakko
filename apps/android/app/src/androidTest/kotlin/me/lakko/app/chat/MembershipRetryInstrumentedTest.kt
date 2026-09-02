// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiRefusalReason
import uniffi.lakko_ffi_uniffi.membershipRefusalRetryMaySucceed

/**
 * ADR-0045 UX §7 — arming against the REAL core.
 *
 * These call `liblakko_ffi_uniffi.so` (`membershipRefusalRetryMaySucceed`), so
 * they run inside the emulator / device process rather than the host JVM — the
 * same split [MembershipRefusalInstrumentedTest] uses and this file inherits.
 *
 * **What this half exists to catch, and what it cannot.** The host suite
 * [MembershipRetryTest] drives `armedRetryFor` through a port that STATES the
 * maintainer's rule, so it proves the function *asks* rather than deciding — a
 * version with a hard-coded partition fails it. It cannot prove the stated
 * rule is still the core's. That is what this file does.
 *
 * Symmetrically, this file could not notice a hard-coded partition: it would
 * agree with the core on every input.
 */
@RunWith(AndroidJUnit4::class)
class MembershipRetryInstrumentedTest {

    /** The real core port — what `ChatViewModel` passes in production. */
    private val real: (FfiRefusalReason) -> Boolean = ::membershipRefusalRetryMaySucceed

    private val addCarol =
        RetryableRequest.Add(subjectKeyB64 = "Q0FST0w=", subjectAccountIdB64 = "Y2Fyb2w=")

    @Test
    fun armsExactlyOnTheReasonsTheCoreCallsRetryable() {
        for (reason in FfiRefusalReason.entries) {
            val armed =
                armedRetryFor(
                    error = FfiException.Group("prose the client never reads"),
                    request = addCarol,
                    pendingCountAtRefusal = 64,
                    explicitReason = reason,
                    retryMaySucceed = real,
                )
            if (real(reason)) {
                assertNotNull("$reason is retryable per the core, so it must arm", armed)
            } else {
                assertNull("$reason is not retryable per the core, so it must not arm", armed)
            }
        }
    }

    /**
     * The approve arm, against the real core. The host suite proves it ASKS;
     * this proves the answers it gets are the core's own — including that the
     * queue-full approve refusal the core stopped flattening into prose is one
     * the core still calls retryable.
     */
    @Test
    fun theApproveArmsExactlyOnTheReasonsTheCoreCallsRetryable() {
        for (reason in FfiRefusalReason.entries) {
            val armed =
                armedApproveJoinRetry(
                    error = FfiException.Group("prose the client never reads"),
                    subjectKeyB64 = "Q0FST0w=",
                    pendingCountAtRefusal = 64,
                    retained = true,
                    explicitReason = reason,
                    retryMaySucceed = real,
                )
            if (real(reason)) {
                assertNotNull("$reason is retryable per the core, so it must arm", armed)
            } else {
                assertNull("$reason is not retryable per the core, so it must not arm", armed)
            }
        }
    }

    /** An unretained request never arms, whatever the core says about the reason. */
    @Test
    fun theApproveNeverArmsForARequestTheQuarantineDidNotKeep() {
        for (reason in FfiRefusalReason.entries) {
            assertNull(
                "$reason must not arm for an unretained request",
                armedApproveJoinRetry(
                    error = FfiException.Group("prose"),
                    subjectKeyB64 = "Q0FST0w=",
                    pendingCountAtRefusal = 64,
                    retained = false,
                    explicitReason = reason,
                    retryMaySucceed = real,
                ),
            )
        }
    }

    @Test
    fun readsTheReasonOffARealCoreExceptionAndArmsOnIt() {
        // Not a stated reason: the exception itself carries it, through the
        // same `membershipRefusalReason` bridge production uses.
        assertNotNull(
            armedRetryFor(
                error = FfiException.MembershipPendingSetFull("core prose"),
                request = addCarol,
                pendingCountAtRefusal = 64,
                retryMaySucceed = real,
            ),
        )
        assertNull(
            armedRetryFor(
                error = FfiException.MembershipAddNotOwnerOrAdmin("core prose"),
                request = addCarol,
                pendingCountAtRefusal = 64,
                retryMaySucceed = real,
            ),
        )
    }
}
