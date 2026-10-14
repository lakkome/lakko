// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Where a call's sound comes out (`docs/ux/calls.md` section 4.1). There is
 * no Speaker control, so the route is a rule: these are its cases.
 */
class CallAudioRouteTest {
    private val earpiece = CallAudioOutput(id = 1, kind = CallOutputKind.EARPIECE, name = "")
    private val speaker = CallAudioOutput(id = 2, kind = CallOutputKind.LOUDSPEAKER, name = "")
    private val wired = CallAudioOutput(id = 5, kind = CallOutputKind.WIRED, name = "Headphones")
    private val buds = CallAudioOutput(id = 7, kind = CallOutputKind.BLUETOOTH, name = "Buds")
    private val phone = listOf(earpiece, speaker)

    // ---- Which calls play on the loudspeaker ---------------------------------

    @Test
    fun `a group call and any call with video play on the loudspeaker`() {
        assertTrue(callPlaysOnLoudspeaker(groupCall = true, videoPresent = false))
        assertTrue(callPlaysOnLoudspeaker(groupCall = true, videoPresent = true))
        assertTrue(callPlaysOnLoudspeaker(groupCall = false, videoPresent = true))
    }

    @Test
    fun `a one-to-one voice call plays on the earpiece`() {
        assertFalse(callPlaysOnLoudspeaker(groupCall = false, videoPresent = false))
        assertEquals(earpiece, callOutputTarget(phone, selectedId = null, loudspeaker = false))
        // A tablet has no earpiece: the platform's default is left alone.
        assertNull(callOutputTarget(listOf(speaker), selectedId = null, loudspeaker = false))
    }

    // ---- A loudspeaker call --------------------------------------------------

    @Test
    fun `a loudspeaker call uses the loudspeaker when nothing is connected`() {
        assertEquals(speaker, callOutputTarget(phone, selectedId = null, loudspeaker = true))
    }

    @Test
    fun `a headset or a Bluetooth device takes a loudspeaker call over by itself`() {
        assertEquals(buds, callOutputTarget(phone + buds, selectedId = null, loudspeaker = true))
        assertEquals(wired, callOutputTarget(phone + wired, selectedId = null, loudspeaker = true))
        // Both there: the wire, the more deliberate act.
        assertEquals(wired, callOutputTarget(phone + buds + wired, selectedId = null, loudspeaker = true))
    }

    @Test
    fun `unplugging the headset gives the call back to the loudspeaker`() {
        assertEquals(buds, callOutputTarget(phone + buds, selectedId = null, loudspeaker = true))
        assertEquals(speaker, callOutputTarget(phone, selectedId = null, loudspeaker = true))
    }

    @Test
    fun `with no loudspeaker the earpiece is used, and with nothing there is nothing to choose`() {
        assertEquals(CallOutputKind.EARPIECE, groupCallOutput(setOf(CallOutputKind.EARPIECE)))
        assertNull(groupCallOutput(emptySet()))
        assertNull(callOutputTarget(emptyList(), selectedId = null, loudspeaker = true))
    }

    // ---- The person's own choice ---------------------------------------------

    @Test
    fun `an output chosen in Settings wins over the rule, for either kind of call`() {
        assertEquals(earpiece, callOutputTarget(phone + buds, selectedId = 1, loudspeaker = true))
        assertEquals(speaker, callOutputTarget(phone, selectedId = 2, loudspeaker = false))
        assertEquals(buds, callOutputTarget(phone + buds, selectedId = 7, loudspeaker = false))
    }

    @Test
    fun `a chosen output that is gone falls back to the rule`() {
        assertEquals(speaker, callOutputTarget(phone, selectedId = 7, loudspeaker = true))
        assertEquals(earpiece, callOutputTarget(phone, selectedId = 7, loudspeaker = false))
    }

    // ---- Where the platform cannot name a device -----------------------------

    @Test
    fun `below Android 12 the earpiece is not offered while a wire is plugged in`() {
        // With the loudspeaker and Bluetooth both off the platform itself picks
        // the wire, so a row for the earpiece would be a choice the app cannot
        // honour.
        assertEquals(listOf(speaker, wired, buds), legacySelectableOutputs(listOf(earpiece, speaker, wired, buds)))
        assertEquals(phone, legacySelectableOutputs(phone))
    }

    // ---- Microphones ---------------------------------------------------------

    @Test
    fun `a phone's several built-in microphones are one row`() {
        val bottom = CallAudioInput(id = 3, kind = CallInputKind.BUILT_IN, name = "")
        val back = CallAudioInput(id = 4, kind = CallInputKind.BUILT_IN, name = "")
        val headset = CallAudioInput(id = 9, kind = CallInputKind.WIRED, name = "Headset")
        assertEquals(listOf(bottom), selectableInputs(listOf(bottom, back)))
        assertEquals(listOf(bottom, headset), selectableInputs(listOf(bottom, headset, back)))
        assertTrue(selectableInputs(emptyList()).isEmpty())
    }
}
