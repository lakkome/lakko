// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import android.Manifest
import org.junit.Assert.assertEquals
import org.junit.Test

/** The two pure rules behind the just-in-time call permission request. */
class CallPermissionsTest {

    @Test
    fun `no microphone means no call, whatever else was granted`() {
        assertEquals(
            CallPermissionOutcome.MICROPHONE_DENIED,
            callPermissionOutcome(microphoneGranted = false, videoRequested = false, cameraGranted = true),
        )
        assertEquals(
            CallPermissionOutcome.MICROPHONE_DENIED,
            callPermissionOutcome(microphoneGranted = false, videoRequested = true, cameraGranted = true),
        )
    }

    @Test
    fun `a refused camera blocks only a video call`() {
        assertEquals(
            CallPermissionOutcome.CAMERA_DENIED,
            callPermissionOutcome(microphoneGranted = true, videoRequested = true, cameraGranted = false),
        )
        assertEquals(
            CallPermissionOutcome.PROCEED,
            callPermissionOutcome(microphoneGranted = true, videoRequested = false, cameraGranted = false),
        )
        assertEquals(
            CallPermissionOutcome.PROCEED,
            callPermissionOutcome(microphoneGranted = true, videoRequested = true, cameraGranted = true),
        )
    }

    @Test
    fun `only missing permissions are requested and never the camera for audio`() {
        assertEquals(
            listOf(Manifest.permission.RECORD_AUDIO),
            callPermissionsToRequest(microphoneGranted = false, cameraGranted = false, videoRequested = false),
        )
        assertEquals(
            listOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.CAMERA),
            callPermissionsToRequest(microphoneGranted = false, cameraGranted = false, videoRequested = true),
        )
        assertEquals(
            listOf(Manifest.permission.CAMERA),
            callPermissionsToRequest(microphoneGranted = true, cameraGranted = false, videoRequested = true),
        )
        assertEquals(
            emptyList<String>(),
            callPermissionsToRequest(microphoneGranted = true, cameraGranted = true, videoRequested = true),
        )
    }
}
