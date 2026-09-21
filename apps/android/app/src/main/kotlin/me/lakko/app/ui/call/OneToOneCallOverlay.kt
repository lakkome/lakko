// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.OneToOneCall
import me.lakko.app.calls.OneToOneCallHost
import me.lakko.app.calls.OneToOneCallPresentation
import me.lakko.app.calls.offeredCallMedia
import me.lakko.app.calls.oneToOneCallPresentation

/**
 * The app-wide presentation of the 1x1 call, mounted once above every session
 * screen by `MainActivity` (chat list, contacts, home, any chat).
 *
 *  - An incoming call that has not been answered is the full-screen incoming
 *    call over whatever screen is showing (`docs/ux/calls.md` section 2.2):
 *    Decline, Accept, and for a video offer an Accept-without-video between
 *    them, since an answer cannot add a media section the offer does not have.
 *  - An outgoing VIDEO call that has not been dialled yet is the camera check
 *    of `docs/ux/calls.md` section 3.2. A voice call never reaches that state:
 *    the host dials it as it prepares it (section 3.1).
 *  - Everything else — a call being set up or live, the ended state — is the
 *    full-screen in-call screen, as a dialog so it covers the screen's own
 *    dialogs too. Its only exit is the in-screen End button.
 *
 * The call lives in [OneToOneCallHost]; this composable only renders it, so
 * navigating between screens never ends a call.
 *
 * @param nameForGroup the other person's resolved name for a 1x1 chat, or `null`.
 */
@Composable
public fun BoxScope.OneToOneCallOverlay(
    host: OneToOneCallHost<CallSession>,
    nameForGroup: (groupIdHex: String) -> String?,
) {
    val permissions = rememberCallPermissionGate()
    val active by host.active.collectAsStateWithLifecycle()
    val call = active ?: return
    val state by call.session.state.collectAsStateWithLifecycle()
    val name = nameForGroup(call.groupIdHex)
    when (oneToOneCallPresentation(state, call.accepted)) {
        // docs/ux/calls.md section 2.1, first row: an app that is open shows
        // the full-screen incoming call, not a strip at the top. The ring
        // notification cannot deliver that on an unlocked phone -- the
        // platform downgrades its full-screen intent to a heads-up -- so this
        // is the only surface that can, and it is the SAME composable the
        // notification's full-screen intent shows when the phone is locked.
        OneToOneCallPresentation.RINGING_SCREEN -> Dialog(
            onDismissRequest = { /* a ring is accepted or declined, never dismissed */ },
            properties = DialogProperties(
                usePlatformDefaultWidth = false,
                dismissOnBackPress = false,
                dismissOnClickOutside = false,
                decorFitsSystemWindows = false,
            ),
        ) {
            EdgeToEdgeDialogWindow()
            IncomingCallContent(
                callerName = name,
                video = call.remoteOffersVideo,
                // The chat's own avatar colour, so the ring, the call screen
                // and the chat row are one person.
                avatarSeed = call.groupIdHex,
                // A choice made on the notification is taken by the activity
                // that notification opens; nothing arrives pre-answered here.
                pendingAnswer = null,
                onAnswerConsumed = {},
                // IncomingCallContent runs the microphone/camera gate itself.
                onAccept = { withVideo -> host.accept(withVideo = withVideo) },
                onDecline = { host.decline() },
            )
        }
        OneToOneCallPresentation.IN_CALL_SCREEN -> Dialog(
            onDismissRequest = { /* exit only via the End button */ },
            properties = DialogProperties(
                usePlatformDefaultWidth = false,
                dismissOnBackPress = false,
                dismissOnClickOutside = false,
                // Full-screen and edge to edge: with the default the dialog is
                // placed below the status bar but keeps the full screen height,
                // so the controls row ran off the bottom edge. The call screen
                // pads itself with the safe-drawing insets.
                decorFitsSystemWindows = false,
            ),
        ) {
            EdgeToEdgeDialogWindow()
            Box(modifier = Modifier.fillMaxSize()) {
                // docs/ux/calls.md sections 3.1 and 3.2. A prepared but
                // undialled OUTGOING call is the one moment anything is shown
                // before the call, and only a video call ever reaches it: the
                // host dials a voice call (and a video call on a camera-less
                // device) in the same breath as preparing it, so the state
                // below is `Outgoing` by the time this composes. An incoming
                // call never reaches it either — "accepting an incoming call
                // never shows any preparation screen" (section 3.3).
                if (state == CallSession.WebCallState.Idle && !call.incoming) {
                    CameraCheckScreen(
                        calleeName = name ?: "Call",
                        avatarSeed = call.groupIdHex,
                        onCall = { mic, cam ->
                            permissions.request(video = cam) { granted ->
                                // A camera the user asked for but the system
                                // refused is not offered: `granted` is the
                                // truth, not `cam`.
                                val media = offeredCallMedia(microphoneOn = mic, cameraOn = granted)
                                host.dial(audio = media.audio, video = media.video)
                            }
                        },
                        onCancel = { host.cancelPrepared() },
                    )
                } else {
                    CallScreen(
                        onExit = { host.clearFinished() },
                        session = call.session,
                        // ADR-0048 UX §5 — the other person's resolved name.
                        remoteName = name,
                        // The chat's own avatar colour, so the portrait of
                        // section 4 matches the row the call was started from.
                        avatarSeed = call.groupIdHex,
                        // docs/ux/calls.md section 4.1 — the flip button on the
                        // self-view rectangle, which is the only place it lives.
                        onSwitchCamera = { call.session.switchCamera() },
                    )
                }
            }
        }
    }
}

