// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Where an incoming 1x1 call rings, and what the app draws while it does.
 *
 * Both rules changed on 2026-09-21 and both changed for the same live defect:
 * with the app OPEN on the chat, the ring arrived as the platform's heads-up
 * strip over a small in-app banner, and the strip folds away after a few
 * seconds, so accepting meant catching it mid-collapse. Four of seven calls on
 * that walk were lost to it (two recorded `Missed call`, one `Declined`).
 * `docs/ux/calls.md` section 2.1 asks for the full-screen incoming call over
 * the app in exactly that case.
 */
class IncomingCallSurfaceTest {

    @Test
    fun `an open app rings itself so the notification cannot cover its screen`() {
        assertEquals(
            RingSurface.IN_APP,
            ringSurface(ringing = true, appInForeground = true, remainingMs = 30_000L),
        )
    }

    @Test
    fun `an app that is away rings through the notification`() {
        assertEquals(
            RingSurface.NOTIFICATION,
            ringSurface(ringing = true, appInForeground = false, remainingMs = 30_000L),
        )
    }

    @Test
    fun `exactly one surface rings at a time`() {
        for (foreground in listOf(true, false)) {
            val surface = ringSurface(ringing = true, appInForeground = foreground, remainingMs = 1L)
            assertEquals(foreground, surface == RingSurface.IN_APP)
            assertEquals(!foreground, surface == RingSurface.NOTIFICATION)
        }
    }

    @Test
    fun `a window that has run out rings nowhere, in front or away`() {
        for (foreground in listOf(true, false)) {
            assertEquals(
                RingSurface.NONE,
                ringSurface(ringing = true, appInForeground = foreground, remainingMs = 0L),
            )
        }
    }

    @Test
    fun `nothing ringing rings nowhere even with time left`() {
        assertEquals(
            RingSurface.NONE,
            ringSurface(ringing = false, appInForeground = true, remainingMs = 45_000L),
        )
    }

    @Test
    fun `an unanswered incoming call is the full-screen ring, not a strip`() {
        assertEquals(
            OneToOneCallPresentation.RINGING_SCREEN,
            oneToOneCallPresentation(CallSession.WebCallState.Ringing, accepted = false),
        )
    }

    @Test
    fun `accepting leaves the ring screen at once`() {
        assertEquals(
            OneToOneCallPresentation.IN_CALL_SCREEN,
            oneToOneCallPresentation(CallSession.WebCallState.Ringing, accepted = true),
        )
    }

    /**
     * `docs/ux/calls.md` section 2.1 — a screen that takes over the phone IS
     * an interruption, so a muted conversation withholds it exactly as it
     * withholds the ringtone. This client briefly did the other thing, which
     * honoured half the rule and was also the one point where it disagreed
     * with the other two clients.
     */
    @Test
    fun `a muted conversation is not interrupted by the ring screen either`() {
        assertEquals(
            OneToOneCallPresentation.NONE,
            oneToOneCallPresentation(
                CallSession.WebCallState.Ringing,
                accepted = false,
                muted = true,
            ),
        )
    }

    /**
     * And mute withholds only the interruption. Once the call is accepted
     * nobody is being interrupted, so the call screen presents normally -
     * otherwise a muted call answered from elsewhere would run invisibly.
     */
    @Test
    fun `a muted call that was accepted presents like any other`() {
        assertEquals(
            OneToOneCallPresentation.IN_CALL_SCREEN,
            oneToOneCallPresentation(
                CallSession.WebCallState.Ringing,
                accepted = true,
                muted = true,
            ),
        )
        assertEquals(
            OneToOneCallPresentation.IN_CALL_SCREEN,
            oneToOneCallPresentation(
                CallSession.WebCallState.Connected,
                accepted = false,
                muted = true,
            ),
        )
    }
}
