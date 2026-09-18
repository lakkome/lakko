// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material.icons.filled.VideocamOff
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.calls.CameraVideoCapture
import me.lakko.app.calls.LocalVideoPreviewView
import me.lakko.app.calls.WebRtcPeerConnection
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.ui.theme.LakkoTheme
import org.webrtc.RendererCommon
import org.webrtc.VideoSource
import org.webrtc.VideoTrack

/**
 * The camera check — `docs/ux/calls.md` section 3.2.
 *
 * ## What it replaces
 *
 * `PrejoinScreen`: a dark card headed "Join call" holding an empty 16:9 box
 * with the words "Camera preview" in it, two settings-list rows with switches
 * on the right, a Join button and a Cancel link. It was shown before **every**
 * call, voice ones included.
 *
 * It was also never a preview. The box only rendered video if the call host had
 * already armed a camera, which on the outgoing path it never had — so the box
 * sat empty for the whole time the screen was up, and the user was asked to
 * decide about a camera they could not see.
 *
 * ## What this is
 *
 * "It fills the screen with the live self-view, the callee's name across the
 * top, and three round controls above the bottom edge: microphone, camera, and
 * a large Call button between them. Cancel is a plain close control in the top
 * corner. No card, no toggle rows, no labels reading like settings."
 *
 * The self-view is live because this screen acquires the camera itself, with no
 * peer connection and no call: [WebRtcPeerConnection.newPreviewVideoTrack] over
 * a [CameraVideoCapture] it owns and releases in its own `onDispose`. The camera
 * is therefore let go before the call's own capture opens it, which matters —
 * two captures of one camera do not coexist on Android.
 *
 * "Turning the camera off there does not leave a hole: the preview becomes the
 * callee's avatar on the call background, and the button still reads Call."
 */
@Composable
internal fun CameraCheckScreen(
    /** The person being called. Shown across the top, and as the avatar when the
     *  camera is off. */
    calleeName: String,
    /** Stable seed for the avatar tint, so the colour matches the chat's. */
    avatarSeed: String,
    /** Place the call with these choices. */
    onCall: (micOn: Boolean, cameraOn: Boolean) -> Unit,
    onCancel: () -> Unit,
) {
    var micOn by remember { mutableStateOf(true) }
    var cameraOn by remember { mutableStateOf(true) }

    LakkoTheme(dark = true) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(LakkoDark.background),
        ) {
            if (cameraOn) {
                SelfViewPreview(modifier = Modifier.fillMaxSize())
            } else {
                Box(
                    modifier = Modifier.fillMaxSize(),
                    contentAlignment = Alignment.Center,
                ) {
                    LakkoAvatar(name = calleeName, seed = avatarSeed, size = 132.dp)
                }
            }

            // The name across the top, over a shade so it reads on any frame.
            Column(
                modifier = Modifier
                    .align(Alignment.TopCenter)
                    .fillMaxWidth()
                    .windowInsetsPadding(WindowInsets.safeDrawing),
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Box(
                        modifier = Modifier
                            .size(44.dp)
                            .clip(CircleShape)
                            .clickable(onClick = onCancel)
                            .semantics { contentDescription = "Cancel the call"; role = Role.Button },
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            Icons.Filled.Close,
                            contentDescription = null,
                            tint = LakkoDark.text,
                            modifier = Modifier.size(24.dp),
                        )
                    }
                    Text(
                        text = calleeName,
                        color = LakkoDark.text,
                        fontSize = 20.sp,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f).padding(horizontal = 8.dp),
                    )
                    // Balances the close control so the name sits centred.
                    Spacer(Modifier.width(44.dp))
                }
            }

            // Microphone, Call, camera — round buttons, never switches.
            Row(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .windowInsetsPadding(WindowInsets.safeDrawing)
                    .padding(bottom = 32.dp),
                horizontalArrangement = Arrangement.spacedBy(24.dp),
                verticalAlignment = Alignment.Bottom,
            ) {
                CameraCheckButton(
                    icon = if (micOn) Icons.Filled.Mic else Icons.Filled.MicOff,
                    label = "Mute",
                    // Same rule as in a call (section 4.1.1): off is the loud state.
                    solid = !micOn,
                    spoken = if (micOn) "Mute microphone" else "Unmute microphone",
                    onClick = { micOn = !micOn },
                )
                CameraCheckButton(
                    icon = Icons.Filled.Call,
                    label = "Call",
                    diameter = 76.dp,
                    fill = LakkoDark.success,
                    foreground = LakkoDark.background,
                    spoken = if (cameraOn) "Place video call" else "Place call",
                    onClick = { onCall(micOn, cameraOn) },
                )
                CameraCheckButton(
                    icon = if (cameraOn) Icons.Filled.Videocam else Icons.Filled.VideocamOff,
                    label = "Camera",
                    solid = !cameraOn,
                    spoken = if (cameraOn) "Turn camera off" else "Turn camera on",
                    onClick = { cameraOn = !cameraOn },
                )
            }
        }
    }
}

/**
 * A live front-camera preview owned by this composable alone.
 *
 * Everything is torn down in `onDispose` — the capture first, then the track,
 * then the source — because the call that follows opens the same camera and
 * Android gives it to one client at a time.
 */
@Composable
private fun SelfViewPreview(modifier: Modifier = Modifier) {
    val context = LocalContext.current
    var track by remember { mutableStateOf<VideoTrack?>(null) }

    DisposableEffect(Unit) {
        var source: VideoSource? = null
        var capture: CameraVideoCapture? = null
        var created: VideoTrack? = null
        // A camera that will not open leaves `track` null, and the box below
        // shows nothing rather than a broken surface. The call can still be
        // placed: this screen is a check, not a gate.
        runCatching {
            source = WebRtcPeerConnection.newVideoSource(context)
            val egl = WebRtcPeerConnection.sharedEglContext()
            val videoSource = source
            if (videoSource != null && egl != null) {
                capture = CameraVideoCapture.create(context, videoSource, egl)
                capture?.start()
                created = WebRtcPeerConnection.newPreviewVideoTrack(context, videoSource)
                track = created
            }
        }.onFailure { android.util.Log.w(TAG, "camera check preview failed to start", it) }

        onDispose {
            track = null
            runCatching { capture?.stop() }
            runCatching { created?.dispose() }
            runCatching { capture?.dispose() }
            runCatching { source?.dispose() }
        }
    }

    Box(modifier = modifier.background(LakkoDark.background)) {
        val egl = remember { WebRtcPeerConnection.sharedEglContext() }
        val current = track
        if (current != null && egl != null) {
            LocalVideoPreviewView(
                track = current,
                eglContext = egl,
                modifier = Modifier.fillMaxSize(),
                mirrored = true,
                scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
            )
        }
    }
}

@Composable
private fun CameraCheckButton(
    icon: ImageVector,
    label: String,
    spoken: String,
    onClick: () -> Unit,
    solid: Boolean = false,
    diameter: androidx.compose.ui.unit.Dp = 64.dp,
    fill: Color? = null,
    foreground: Color? = null,
) {
    val background = fill ?: if (solid) LakkoDark.text else LakkoDark.text.copy(alpha = 0.12f)
    val tint = foreground ?: if (solid) LakkoDark.background else LakkoDark.text
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = Modifier.semantics(mergeDescendants = true) {
            contentDescription = spoken
            role = Role.Button
        },
    ) {
        Box(
            modifier = Modifier
                .size(diameter)
                .clip(CircleShape)
                .background(background)
                .clickable(onClick = onClick),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                imageVector = icon,
                contentDescription = null,
                tint = tint,
                modifier = Modifier.size(28.dp),
            )
        }
        Spacer(Modifier.height(6.dp))
        Text(
            text = label,
            color = LakkoDark.textMuted,
            fontSize = 12.sp,
            fontWeight = FontWeight.Medium,
            modifier = Modifier.clearAndSetSemantics { },
        )
    }
}

private const val TAG = "CameraCheckScreen"
