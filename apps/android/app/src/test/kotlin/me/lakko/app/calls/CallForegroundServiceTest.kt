// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.Manifest
import android.app.Application
import android.app.Notification
import android.content.Intent
import android.content.pm.ServiceInfo
import androidx.test.core.app.ApplicationProvider
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

/**
 * `docs/ux/calls.md` section 4.5.4 — the call's foreground service and its
 * ongoing notification. What Robolectric can show: the types asked for, and
 * the notification the service puts up. Whether the platform then keeps the
 * call's sockets, microphone and camera alive needs a device.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class CallForegroundServiceTest {
    init {
        EnglishForTests.install()
        // The process-wide call host loads the native core, which the host
        // JVM does not have; the service waits for nothing here.
        CallForegroundService.callEnded = { kotlinx.coroutines.awaitCancellation() }
    }

    @Test
    fun theMicrophoneTypeIsHeldWheneverItsPermissionIs() {
        assertEquals(
            setOf(OngoingCallServiceType.MICROPHONE),
            ongoingCallServiceTypes(microphoneGranted = true, cameraGranted = true, cameraSending = false),
        )
    }

    @Test
    fun theCameraTypeIsAddedOnlyWhileTheCameraSends() {
        assertEquals(
            setOf(OngoingCallServiceType.MICROPHONE, OngoingCallServiceType.CAMERA),
            ongoingCallServiceTypes(microphoneGranted = true, cameraGranted = true, cameraSending = true),
        )
    }

    @Test
    fun aTypeWhosePermissionIsMissingIsNeverAskedFor() {
        // Android 14 and later throw on a foreground type without its
        // permission, which would end the app rather than keep the call.
        assertEquals(
            emptySet<OngoingCallServiceType>(),
            ongoingCallServiceTypes(microphoneGranted = false, cameraGranted = false, cameraSending = true),
        )
        assertEquals(
            setOf(OngoingCallServiceType.MICROPHONE),
            ongoingCallServiceTypes(microphoneGranted = true, cameraGranted = false, cameraSending = true),
        )
    }

    @Test
    fun theMaskCarriesThePlatformBits() {
        assertEquals(
            ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA,
            ongoingCallServiceTypeMask(setOf(OngoingCallServiceType.MICROPHONE, OngoingCallServiceType.CAMERA)),
        )
    }

    @Test
    fun theServicePutsUpAnOngoingSilentNotificationNamingTheCall() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        shadowOf(app).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val intent = Intent(app, CallForegroundService::class.java)
            .putExtra(OngoingCallNotice.EXTRA_LABEL, "Anna")
            .putExtra(OngoingCallNotice.EXTRA_CAMERA, false)
        val controller = Robolectric.buildService(CallForegroundService::class.java, intent).create().startCommand(0, 1)
        val shadow = shadowOf(controller.get())
        val notification: Notification? = shadow.lastForegroundNotification
        assertNotNull("the service did not go to the foreground", notification)
        val extras = notification!!.extras
        assertEquals("Anna", extras.getString(Notification.EXTRA_TITLE))
        assertEquals("Tap to return to the call", extras.getCharSequence(Notification.EXTRA_TEXT)?.toString())
        assertTrue("the notification can be swiped away", notification.flags and Notification.FLAG_ONGOING_EVENT != 0)
        assertNotNull("tapping it opens nothing", notification.contentIntent)
        controller.destroy()
    }
}
