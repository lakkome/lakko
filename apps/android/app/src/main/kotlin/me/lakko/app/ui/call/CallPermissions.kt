// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import me.lakko.app.l10n.t
import androidx.activity.compose.ManagedActivityResultLauncher
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat

/**
 * Just-in-time runtime permissions for every way into a call.
 *
 * The manifest declares `RECORD_AUDIO` and `CAMERA`, but both are DANGEROUS
 * permissions: declaring them grants nothing, and before this file no call path
 * asked for either. The WebRTC audio source then captured silence (or failed),
 * with nothing on screen to say why. Every call entry point — start, join,
 * accept, the prejoin Join, and a `/c/` call link — now goes through
 * [CallPermissionGate.request] first.
 *
 * The microphone is required: without it there is no call, and the user is told
 * so with a way to the app's settings (a permanently denied permission returns
 * from the system request immediately, so a settings link is the only exit). The
 * camera is required only for video; when it is refused the user chooses between
 * audio only and settings rather than having video silently dropped.
 */

/** What to do once the permission request has come back. */
internal enum class CallPermissionOutcome {
    /** Everything the call needs was granted. */
    PROCEED,

    /** No microphone — the call cannot start. */
    MICROPHONE_DENIED,

    /** A video call without the camera — offer audio only. */
    CAMERA_DENIED,
}

internal fun callPermissionOutcome(
    microphoneGranted: Boolean,
    videoRequested: Boolean,
    cameraGranted: Boolean,
): CallPermissionOutcome = when {
    !microphoneGranted -> CallPermissionOutcome.MICROPHONE_DENIED
    videoRequested && !cameraGranted -> CallPermissionOutcome.CAMERA_DENIED
    else -> CallPermissionOutcome.PROCEED
}

/** The permissions still missing for a call, in request order. Empty when the
 *  call can proceed without asking. The camera is never requested for an audio
 *  call. */
internal fun callPermissionsToRequest(
    microphoneGranted: Boolean,
    cameraGranted: Boolean,
    videoRequested: Boolean,
): List<String> = buildList {
    if (!microphoneGranted) add(Manifest.permission.RECORD_AUDIO)
    if (videoRequested && !cameraGranted) add(Manifest.permission.CAMERA)
}

/** Holds one in-flight request and the denial dialog it may end in. */
@Stable
public class CallPermissionGate internal constructor() {

    private class Pending(
        /** Whether CAMERA is part of THIS system request. */
        val cameraRequested: Boolean,
        /**
         * What the user actually asked for, which survives a device that has no
         * camera to ask about. Keeping the two apart is what stops a
         * camera-less device being told "Camera access needed" for a permission
         * nothing on it can grant — and what lets the video button's own rule
         * ([me.lakko.app.calls.callEntrySurface]) still see a video request and
         * explain the voice call it places instead.
         */
        val requestedVideo: Boolean,
        val onProceed: (video: Boolean) -> Unit,
    )

    internal sealed interface Denial {
        data object Microphone : Denial

        class Camera(val onAudioOnly: () -> Unit) : Denial
    }

    private var pending: Pending? = null
    internal var denial: Denial? by mutableStateOf(null)
    internal var launcher: ManagedActivityResultLauncher<Array<String>, Map<String, Boolean>>? = null
    internal var context: Context? = null

    /**
     * Run [onProceed] once the call has what it needs. [onProceed] receives the
     * video flag to actually use: `false` when the user chose audio only after
     * refusing the camera. It is not called at all when the microphone is
     * refused.
     */
    public fun request(video: Boolean, onProceed: (video: Boolean) -> Unit) {
        val ctx = context ?: return
        // A device with no camera has no camera permission to grant, so asking
        // for one can only end in a dialog telling the user to go and enable
        // something that does not exist.
        val cameraRequested = video && me.lakko.app.calls.CameraVideoCapture.hasUsableCamera(ctx)
        val missing = callPermissionsToRequest(
            microphoneGranted = isGranted(ctx, Manifest.permission.RECORD_AUDIO),
            cameraGranted = isGranted(ctx, Manifest.permission.CAMERA),
            videoRequested = cameraRequested,
        )
        if (missing.isEmpty()) {
            onProceed(video)
            return
        }
        val requester = launcher ?: return
        pending = Pending(cameraRequested = cameraRequested, requestedVideo = video, onProceed = onProceed)
        requester.launch(missing.toTypedArray())
    }

    internal fun onResult() {
        val ctx = context ?: return
        val request = pending ?: return
        pending = null
        // Re-read the grants rather than trusting the result map: the map only
        // names what was asked this time, and the camera may have been granted
        // earlier.
        when (
            callPermissionOutcome(
                microphoneGranted = isGranted(ctx, Manifest.permission.RECORD_AUDIO),
                videoRequested = request.cameraRequested,
                cameraGranted = isGranted(ctx, Manifest.permission.CAMERA),
            )
        ) {
            CallPermissionOutcome.PROCEED -> request.onProceed(request.requestedVideo)
            CallPermissionOutcome.MICROPHONE_DENIED -> denial = Denial.Microphone
            CallPermissionOutcome.CAMERA_DENIED ->
                denial = Denial.Camera(onAudioOnly = { request.onProceed(false) })
        }
    }

    private fun isGranted(ctx: Context, permission: String): Boolean =
        ContextCompat.checkSelfPermission(ctx, permission) == PackageManager.PERMISSION_GRANTED
}

/**
 * A [CallPermissionGate] bound to this composition's activity, with its denial
 * dialogs already mounted — call sites only call [CallPermissionGate.request].
 */
@Composable
public fun rememberCallPermissionGate(): CallPermissionGate {
    val context = LocalContext.current
    val gate = remember { CallPermissionGate() }
    val launcher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.RequestMultiplePermissions(),
    ) { gate.onResult() }
    gate.launcher = launcher
    gate.context = context
    CallPermissionDialogs(gate = gate, context = context)
    return gate
}

@Composable
private fun CallPermissionDialogs(gate: CallPermissionGate, context: Context) {
    when (val denial = gate.denial) {
        null -> Unit
        CallPermissionGate.Denial.Microphone -> AlertDialog(
            onDismissRequest = { gate.denial = null },
            title = { Text(t("call.permission.microphone.title")) },
            text = {
                Text(t("call.permission.microphone.body"))
            },
            confirmButton = {
                TextButton(onClick = {
                    gate.denial = null
                    openAppSettings(context)
                }) { Text(t("chat.record.open_app_settings")) }
            },
            dismissButton = {
                TextButton(onClick = { gate.denial = null }) { Text(t("common.not_now")) }
            },
        )
        is CallPermissionGate.Denial.Camera -> AlertDialog(
            onDismissRequest = { gate.denial = null },
            title = { Text(t("call.permission.camera.title")) },
            text = {
                Text(t("call.permission.camera.body"))
            },
            confirmButton = {
                Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    TextButton(onClick = {
                        gate.denial = null
                        openAppSettings(context)
                    }) { Text(t("chat.record.open_app_settings")) }
                    TextButton(onClick = {
                        gate.denial = null
                        denial.onAudioOnly()
                    }) { Text(t("call.permission.audio_only")) }
                }
            },
            dismissButton = {
                TextButton(onClick = { gate.denial = null }) { Text(t("common.cancel")) }
            },
        )
    }
}

private fun openAppSettings(context: Context) {
    val intent = Intent(
        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
        Uri.fromParts("package", context.packageName, null),
    ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    try {
        context.startActivity(intent)
    } catch (cause: Throwable) {
        android.util.Log.w("CallPermissions", "could not open app settings", cause)
    }
}
