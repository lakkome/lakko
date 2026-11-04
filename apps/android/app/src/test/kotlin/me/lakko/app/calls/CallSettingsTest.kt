// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The call settings sheet (`docs/ux/calls.md` section 4.1): which sections
 * and rows exist for a given set of devices and a given selection. The first
 * half is the web client's test list (`call-settings.test.ts`); the second is
 * what this platform reports and what it lets the app choose.
 */
class CallSettingsTest {
    private val strings = EnglishForTests.install()

    private val devices = listOf(
        SettingsDevice("default", "Default - Phone microphone", SettingsDeviceKind.AUDIO_INPUT),
        SettingsDevice("mic-1", "Phone microphone", SettingsDeviceKind.AUDIO_INPUT),
        SettingsDevice("mic-2", "Headset", SettingsDeviceKind.AUDIO_INPUT),
        SettingsDevice("cam-1", "Front camera", SettingsDeviceKind.VIDEO_INPUT),
        SettingsDevice("out-1", "Speaker", SettingsDeviceKind.AUDIO_OUTPUT),
        SettingsDevice("out-2", "", SettingsDeviceKind.AUDIO_OUTPUT),
    )
    private val none = SettingsSelection()

    private fun sections(
        devices: List<SettingsDevice> = this.devices,
        selection: SettingsSelection = none,
        canSelectAudioOutput: Boolean = true,
    ) = callSettingsSections(devices, selection, canSelectAudioOutput, strings)

    // ---- Which sections and rows exist (the web list) -------------------------

    @Test
    fun `microphone, camera and speaker are listed, each starting with the system default`() {
        val all = sections()
        assertEquals(
            listOf(SettingsSectionKind.MICROPHONE, SettingsSectionKind.CAMERA, SettingsSectionKind.SPEAKER),
            all.map { it.kind },
        )
        assertEquals(listOf("Microphone", "Camera", "Speaker"), all.map { it.title })
        for (section in all) {
            assertEquals(SettingsOption(id = null, label = "System default", selected = true), section.options[0])
        }
    }

    @Test
    fun `an entry that is itself the default is dropped, since the first row already is it`() {
        assertEquals(listOf(null, "mic-1", "mic-2"), sections()[0].options.map { it.id })
    }

    @Test
    fun `the chosen device is marked and nothing else`() {
        val mic = sections(selection = none.copy(audioInput = "mic-2"))[0]
        assertEquals(listOf("mic-2"), mic.options.filter { it.selected }.map { it.id })
    }

    @Test
    fun `a choice the platform no longer reports reads as the system default`() {
        val mic = sections(selection = none.copy(audioInput = "unplugged"))[0]
        assertEquals(listOf<String?>(null), mic.options.filter { it.selected }.map { it.id })
    }

    @Test
    fun `there is no speaker section where the app cannot move the sound`() {
        assertEquals(
            listOf(SettingsSectionKind.MICROPHONE, SettingsSectionKind.CAMERA),
            sections(canSelectAudioOutput = false).map { it.kind },
        )
    }

    @Test
    fun `a kind with no device has no section`() {
        val onlyMic = devices.filter { it.kind == SettingsDeviceKind.AUDIO_INPUT }
        assertEquals(listOf(SettingsSectionKind.MICROPHONE), sections(devices = onlyMic).map { it.kind })
        assertTrue(sections(devices = emptyList()).isEmpty())
    }

    @Test
    fun `a device with no name is numbered`() {
        assertEquals(
            listOf("System default", "Speaker", "Device 2"),
            sections()[2].options.map { it.label },
        )
    }

    // ---- What this platform offers -------------------------------------------

    private val earpiece = CallAudioOutput(id = 1, kind = CallOutputKind.EARPIECE, name = "")
    private val speaker = CallAudioOutput(id = 2, kind = CallOutputKind.LOUDSPEAKER, name = "")
    private val buds = CallAudioOutput(id = 7, kind = CallOutputKind.BLUETOOTH, name = "Buds")
    private val phoneMic = CallAudioInput(id = 3, kind = CallInputKind.BUILT_IN, name = "")
    private val headsetMic = CallAudioInput(id = 9, kind = CallInputKind.WIRED, name = "USB headset")

    @Test
    fun `a phone on its own offers cameras and outputs, and no microphone`() {
        val listed = callSettingsDevices(
            inputs = listOf(phoneMic),
            cameras = listOf("1", "0"),
            outputs = listOf(earpiece, speaker),
            strings = strings,
        )
        val all = sections(devices = listed)
        // One microphone is nothing to choose between.
        assertEquals(listOf(SettingsSectionKind.CAMERA, SettingsSectionKind.SPEAKER), all.map { it.kind })
        // A camera has no name on this platform; the loudspeaker takes the
        // catalog's word and the earpiece, which has none, is numbered.
        assertEquals(listOf("System default", "Device 1", "Device 2"), all[0].options.map { it.label })
        assertEquals(listOf(null, "1", "0"), all[0].options.map { it.id })
        assertEquals(listOf("System default", "Device 1", "Speaker"), all[1].options.map { it.label })
        assertEquals(listOf(null, "1", "2"), all[1].options.map { it.id })
    }

    @Test
    fun `a headset adds its own name to the outputs and a microphone section`() {
        val listed = callSettingsDevices(
            inputs = listOf(phoneMic, headsetMic),
            cameras = listOf("1", "0"),
            outputs = listOf(earpiece, speaker, buds),
            strings = strings,
        )
        val all = sections(devices = listed, selection = SettingsSelection(audioOutput = "7"))
        assertEquals(
            listOf(SettingsSectionKind.MICROPHONE, SettingsSectionKind.CAMERA, SettingsSectionKind.SPEAKER),
            all.map { it.kind },
        )
        assertEquals(listOf("System default", "Microphone", "USB headset"), all[0].options.map { it.label })
        assertEquals(listOf("System default", "Device 1", "Speaker", "Buds"), all[2].options.map { it.label })
        assertEquals(listOf("7"), all[2].options.filter { it.selected }.map { it.id })
    }

    @Test
    fun `a device with one camera and one output has nothing to choose`() {
        val listed = callSettingsDevices(
            inputs = listOf(phoneMic),
            cameras = listOf("0"),
            outputs = listOf(speaker),
            strings = strings,
        )
        assertTrue(listed.isEmpty())
        // The sheet then says so in one sentence rather than showing empty lists.
        assertTrue(sections(devices = listed).isEmpty())
    }

    // ---- The choice holds for the call ---------------------------------------

    @Test
    fun `a choice is the call's and a new call starts from the defaults`() {
        val state = CallUiState()
        state.devices.selectCamera("0")
        state.devices.selectAudioOutput(7)
        state.devices.selectAudioInput(9)
        assertEquals("0", state.devices.camera.value)
        state.reset()
        assertNull(state.devices.camera.value)
        assertNull(state.devices.audioOutput.value)
        assertNull(state.devices.audioInput.value)
    }

    @Test
    fun `a choice held for a one-to-one call outlives the screen's own state`() {
        // The one-to-one screen builds its state anew every time the call
        // comes back from the bar, and resets it when it leaves.
        val held = CallDeviceSelection()
        val first = CallUiState(devices = held)
        first.devices.selectAudioOutput(2)
        first.reset()
        val second = CallUiState(devices = held)
        assertEquals(2, second.devices.audioOutput.value)
    }

    @Test
    fun `with one of everything the sheet lists them instead of saying none were reported`() {
        val listed = callSettingsSheetDevices(listOf(phoneMic), listOf("1"), listOf(speaker), strings)
        assertEquals(
            listOf(SettingsSectionKind.MICROPHONE, SettingsSectionKind.CAMERA, SettingsSectionKind.SPEAKER),
            sections(devices = listed).map { it.kind },
        )
        // A choice somewhere keeps the rule: a kind with one device stays out.
        val choice = callSettingsSheetDevices(listOf(phoneMic), listOf("1", "0"), listOf(speaker), strings)
        assertEquals(listOf(SettingsSectionKind.CAMERA), sections(devices = choice).map { it.kind })
        // Nothing reported is still nothing.
        assertTrue(callSettingsSheetDevices(emptyList(), emptyList(), emptyList(), strings).isEmpty())
    }
}
