// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 2.1 — the phone must ring when the app is
 * backgrounded, closed or locked.
 *
 * The defect these tests state as a rule, verified on an API 35 emulator on
 * 2026-09-23: `POST_NOTIFICATIONS` was declared in the manifest and requested
 * by nothing, so revoking it and relaunching produced no prompt, the
 * incoming-call notification was refused, and the phone silently never rang.
 */
class NotificationPermissionTest {

    private val modern = 35
    private val legacy = 32

    @Test
    fun `a fresh install on a modern phone is asked`() {
        assertEquals(
            NotificationPermissionAsk.Ask,
            notificationPermissionAsk(sdkInt = modern, granted = false, askedBefore = false),
        )
    }

    // ---------------------------------------------------------------------
    // The one that matters: never a second prompt
    // ---------------------------------------------------------------------

    @Test
    fun `an install that asked and was refused does not ask again`() {
        // Android permanently auto-denies after two dismissals: the launcher
        // returns "not granted" with nothing on screen. A second ask would
        // show the person nothing and spend the last chance the platform
        // allows. System settings is where a refusal gets changed.
        assertEquals(
            NotificationPermissionAsk.AlreadyAsked,
            notificationPermissionAsk(sdkInt = modern, granted = false, askedBefore = true),
        )
    }

    @Test
    fun `refused and asked again is never the ask outcome, at any sdk`() {
        for (sdk in 26..40) {
            assertFalse(
                "sdk $sdk re-asked a refusal",
                notificationPermissionAsk(sdk, granted = false, askedBefore = true) ==
                    NotificationPermissionAsk.Ask,
            )
        }
    }

    // ---------------------------------------------------------------------
    // The outcomes that are not an ask, kept distinct from one another
    // ---------------------------------------------------------------------

    @Test
    fun `an already-granted install is not asked`() {
        assertEquals(
            NotificationPermissionAsk.AlreadyGranted,
            notificationPermissionAsk(sdkInt = modern, granted = true, askedBefore = false),
        )
    }

    @Test
    fun `granted wins over asked-before, so a later grant is not reported as a refusal`() {
        // The person asked, refused, then turned notifications on in system
        // settings. Settings must stop saying they are off.
        assertEquals(
            NotificationPermissionAsk.AlreadyGranted,
            notificationPermissionAsk(sdkInt = modern, granted = true, askedBefore = true),
        )
    }

    @Test
    fun `below api 33 there is nothing to ask for`() {
        assertEquals(
            NotificationPermissionAsk.NotNeeded,
            notificationPermissionAsk(sdkInt = legacy, granted = false, askedBefore = false),
        )
        assertEquals(
            NotificationPermissionAsk.NotNeeded,
            notificationPermissionAsk(sdkInt = legacy, granted = true, askedBefore = true),
        )
    }

    @Test
    fun `the runtime permission starts exactly at api 33`() {
        assertEquals(33, NOTIFICATION_PERMISSION_SDK)
        assertEquals(
            NotificationPermissionAsk.NotNeeded,
            notificationPermissionAsk(sdkInt = 32, granted = false, askedBefore = false),
        )
        assertEquals(
            NotificationPermissionAsk.Ask,
            notificationPermissionAsk(sdkInt = 33, granted = false, askedBefore = false),
        )
    }

    @Test
    fun `the rule is total over every combination`() {
        for (sdk in 21..40) {
            for (granted in listOf(false, true)) {
                for (asked in listOf(false, true)) {
                    val decision = notificationPermissionAsk(sdk, granted, asked)
                    val shouldAsk = sdk >= NOTIFICATION_PERMISSION_SDK && !granted && !asked
                    assertEquals(
                        "sdk=$sdk granted=$granted asked=$asked",
                        shouldAsk,
                        decision == NotificationPermissionAsk.Ask,
                    )
                }
            }
        }
    }

    // ---------------------------------------------------------------------
    // The copy Settings shows when the answer was no
    // ---------------------------------------------------------------------

    @Test
    fun `the notice says what happened and what to do, in plain words`() {
        assertEquals("Turn on notifications", NOTIFICATIONS_OFF_TITLE)
        assertTrue(
            "the notice must say what happened",
            NOTIFICATIONS_OFF_NOTICE.startsWith("Notifications are off"),
        )
        assertTrue(
            "the notice must say what to do",
            NOTIFICATIONS_OFF_NOTICE.contains("Turn them on"),
        )
    }

    @Test
    fun `the notice promises only what the app does`() {
        // Lakko rings while it is open whatever the person chose; notifications
        // are what extend that to when it is not. Anything stronger than that
        // would be a promise the app cannot keep.
        assertTrue(NOTIFICATIONS_OFF_NOTICE.contains("only ring while it is open"))
    }

    @Test
    fun `the notice obeys the copy rules`() {
        val banned = listOf(
            "POST_NOTIFICATIONS", "permission", "runtime", "API", "SDK", "push",
            "FCM", "notification channel", "SecurityException",
        )
        for (word in banned) {
            assertFalse(
                "notice leaks \"$word\"",
                NOTIFICATIONS_OFF_NOTICE.contains(word, ignoreCase = true),
            )
            assertFalse(
                "title leaks \"$word\"",
                NOTIFICATIONS_OFF_TITLE.contains(word, ignoreCase = true),
            )
        }
        assertFalse("no exclamation marks", NOTIFICATIONS_OFF_NOTICE.contains("!"))
        assertFalse("no exclamation marks", NOTIFICATIONS_OFF_TITLE.contains("!"))
    }
}
