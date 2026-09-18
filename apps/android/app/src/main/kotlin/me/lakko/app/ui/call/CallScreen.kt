// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentSize
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items as gridItems
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.TextButton
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Divider
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.material.icons.Icons
import androidx.compose.runtime.key
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.CallEnd
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Group
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material.icons.filled.ScreenShare
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material.icons.filled.VideocamOff
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.Chat
import androidx.compose.material.icons.filled.FlipCameraAndroid
import androidx.compose.material.icons.filled.Settings
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.roundToInt
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import androidx.compose.ui.platform.LocalContext
import me.lakko.app.calls.CALL_CONTROLS_AUTO_HIDE_MS
import me.lakko.app.calls.CallAudioRouter
import me.lakko.app.calls.CallControlKind
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.CallStage
import me.lakko.app.calls.CameraVideoCapture
import me.lakko.app.calls.callControlRow
import me.lakko.app.calls.callScreenTitle
import me.lakko.app.calls.callStage
import me.lakko.app.calls.controlsMayAutoHide
import me.lakko.app.calls.showsSelfViewPip
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.theme.avatarTintFor
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.ui.theme.LakkoTheme
import androidx.compose.material.icons.filled.FrontHand
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import me.lakko.app.calls.CALL_ENCRYPTION_CHIP_LABEL
import me.lakko.app.calls.CALL_ENCRYPTION_SHEET_HINT
import me.lakko.app.calls.CALL_ENCRYPTION_SHEET_TITLE
import me.lakko.app.calls.CallSoundController
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.LocalVideoPreviewView
import me.lakko.app.calls.callEncryptionSentence
import me.lakko.app.calls.callStatusSpokenLabel
import me.lakko.app.calls.callStatusText
import me.lakko.app.calls.safetyNumberGroups
import me.lakko.app.calls.safetyNumberSpokenLabel
import me.lakko.app.calls.showsCallEncryptionChip
import me.lakko.app.calls.WebRtcPeerConnection
import org.webrtc.RendererCommon
import org.webrtc.VideoTrack

/**
 * In-call UI (Phase 3c6-android).
 *
 * Compose mirror of `apps/web/src/routes/Call.tsx` +
 * `apps/ios/Lakko/UI/Call/CallScreen.swift`. Same layout, same
 * controls, same security UX (E2EE pill, quality dot popover,
 * mute overlay, waiting message, prejoin screen).
 *
 * State lives in [CallUiState] — the Kotlin port of the Solid
 * signals state machine. Compose subscribes via
 * `collectAsState` (kept inline; Compose recomposes when the
 * StateFlow emits).
 *
 * No real WebRTC integration here — the live media path lives
 * in the platform's WebRTC pod (3a-pod-followup). Tiles render
 * a gradient placeholder until the pod wires real
 * `SurfaceView` / `TextureView`-backed video surfaces in.
 */

// Calls are dark by design (docs/ux/client-redesign.md section 4): the
// colours are the dark half of the one Lakko token set, read straight from
// `LakkoDark`; the screen is wrapped in `LakkoTheme(dark = true)` so every
// Material component inside follows.

private fun CallUiState.QualityTier.colour(): Color = when (this) {
    CallUiState.QualityTier.Excellent -> LakkoDark.success
    CallUiState.QualityTier.Good, CallUiState.QualityTier.Fair -> LakkoDark.warning
    CallUiState.QualityTier.Poor, CallUiState.QualityTier.Bad -> LakkoDark.danger
}

// ============================================================
// CallScreen — top-level composable
// ============================================================

@Composable
public fun CallScreen(
    onExit: () -> Unit,
    /**
     * The call state to render, or `null` to own a fresh one.
     *
     * The distinction is load-bearing and used to be invisible: this screen
     * resets the state in its `onDispose`, which is right for a state it
     * created and destructive for one a host owns and keeps feeding. A live
     * group call hit exactly that — the screen was disposed mid-call, reset the
     * host's state, and the call fell back to `Calling...` with its controls
     * greyed and its self-view gone while the peer's timer ran on, because only
     * this device's VIEW of the call had been wiped. Passing the state now
     * implies the caller owns it.
     */
    state: CallUiState? = null,
    // G3-2-session-bind — the live call session driving this
    // screen. When `null` (Compose previews / design work, and
    // the group + call-link hosts, which drive the screen
    // through [CallUiState] alone) the screen renders whatever
    // the state holds. When injected, `driveFrom` wires phase +
    // roster + video to the real call and the end-call button
    // routes to `session.hangUp()`.
    session: CallSession? = null,
    /** ADR-0048 — the remote participant's resolved name for a 1x1 session. */
    remoteName: String? = null,
    /**
     * Stable seed for the other person's avatar tint, so the portrait of
     * `docs/ux/calls.md` section 4 wears the same colour as their row in the
     * chat list. Falls back to the name, which is stable enough to look
     * deliberate and is all a mock has.
     */
    avatarSeed: String? = null,
    /** The End button for a call the app-level host drives through [state]
     *  alone (a group or call-link call): the host leaves the call and the
     *  session's own closed transition shows the ended state. `null` keeps the
     *  session hang-up (1x1) or, with no session, the mock end. */
    onEndCall: (() -> Unit)? = null,
    /**
     * `docs/ux/calls.md` section 4.1 — flip to the other camera. It lives on
     * the self-view rectangle, not in the control row, so this is wired only
     * where a session can actually swap the capture; `null` leaves the button
     * off the self-view entirely.
     */
    onSwitchCamera: (() -> Unit)? = null,
    /**
     * This is a group or call-link call, whatever the roster happens to hold
     * right now. It keeps the grid, the roster drawer and the in-call chat on
     * screen while the second participant is still arriving; see [callStage].
     */
    groupCall: Boolean = false,
    /**
     * `docs/ux/calls.md` section 4 — what the call is called. For a group call
     * this is the group's name (ADR-0051); `null` means the group has none. It
     * is never derived from a participant: see [callScreenTitle].
     */
    groupName: String? = null,
) {
    // Captured once: whether this composition created the state, and may
    // therefore reset it. Reading the parameter later would be wrong — the
    // question is who made it, which cannot change.
    val ownsState = remember { state == null }
    val callState = state ?: remember { CallUiState() }
    // One sound controller per CallScreen lifetime; released
    // in the same DisposableEffect that resets the UI state so
    // the SoundPool + cached MediaPlayer handles do not leak
    // their AudioTrack resources when the user navigates away.
    val context = LocalContext.current
    val sounds = remember(context) { CallSoundController(context) }
    // docs/ux/calls.md section 4.1 — the Speaker button's other half. Nothing
    // in the app had ever put the audio stream into communication mode, so a
    // call played out of whatever route the system happened to pick and there
    // was no way to change it.
    val audio = remember(context) { CallAudioRouter(context) }
    val speakerOn by audio.speakerOn.collectAsState()
    // ADR-0032 piece B2b — scope for the mid-call camera-on
    // (`session.addLocalVideo()` is a suspend function launched from the
    // camera button's callback). Tied to this composition; torn down with
    // the screen.
    val cameraScope = rememberCoroutineScope()
    DisposableEffect(Unit) {
        audio.start()
        onDispose {
            audio.stop()
            sounds.release()
            // Only what this composition made. A host's state outlives the
            // screen by design, and resetting it here is what turned a
            // disposal into a call that looked dead.
            if (ownsState) callState.reset()
        }
    }

    // The self-view's flip button is drawn only where there is somewhere to
    // flip to; a single-camera device gets no button rather than a dead one.
    LaunchedEffect(context) {
        callState.setHasMultipleCameras(CameraVideoCapture.cameraCount(context) > 1)
    }

    // G3-2-session-bind — drive the UI from the real session
    // when one is injected; otherwise the state is driven by
    // whichever host owns it.
    LaunchedEffect(session) {
        if (session != null) {
            callState.driveFrom(session, remoteName = remoteName.orEmpty())
        }
    }

    val phase by callState.phase.collectAsState()
    val participants by callState.participants.collectAsState()

    // Phase → sound mapping. Mirrors the same trigger map on
    // web (`Call.tsx`) and iOS (`CallScreen.swift`) so the
    // audible UX is identical across platforms. `Calling` took
    // `Prejoin`'s place in the sealed class and rings: it is a
    // dialled call waiting to be answered, which is exactly when
    // a caller expects to hear a ringback (section 3.1).
    LaunchedEffect(phase) {
        when (phase) {
            is CallUiState.CallPhase.Calling,
            is CallUiState.CallPhase.Connecting,
            ->
                sounds.startLoop(CallSoundController.SoundName.RINGBACK_OUTGOING)
            is CallUiState.CallPhase.Connected -> {
                sounds.stopLoop(CallSoundController.SoundName.RINGBACK_OUTGOING)
                sounds.playOnce(CallSoundController.SoundName.JOIN)
            }
            is CallUiState.CallPhase.Reconnecting ->
                sounds.stopLoop(CallSoundController.SoundName.RINGBACK_OUTGOING)
            is CallUiState.CallPhase.Ended -> {
                sounds.stopAll()
                sounds.playOnce(CallSoundController.SoundName.LEAVE)
            }
        }
    }

    // Peer arrival / departure cues + chat system events.
    // id-based diff so "Alice left, Bob joined" in the same
    // StateFlow tick fires both events. Local participant is
    // excluded — they're not a "peer" from the user's POV.
    val remoteIds = participants.filter { !it.isLocal }.map { it.id }
    var lastRemoteIds by remember { mutableStateOf<List<String>>(emptyList()) }
    LaunchedEffect(remoteIds, phase) {
        if (phase is CallUiState.CallPhase.Connected && lastRemoteIds.isNotEmpty()) {
            val remoteParts = participants.filter { !it.isLocal }
            for (p in remoteParts) {
                if (p.id !in lastRemoteIds) {
                    sounds.playOnce(CallSoundController.SoundName.JOIN)
                    callState.addSystemEvent("${p.displayName} joined the call")
                }
            }
            val currentIds = remoteIds.toSet()
            for (prevId in lastRemoteIds) {
                if (prevId !in currentIds) {
                    sounds.playOnce(CallSoundController.SoundName.LEAVE)
                    callState.addSystemEvent("A participant left the call")
                    // Display name has been dropped from state
                    // by the time we observe the deletion;
                    // future refinement preserves it. Out of
                    // scope for S-3-A-chat.
                }
            }
        }
        lastRemoteIds = remoteIds
    }

    // There is no branch on the phase any more. `docs/ux/calls.md` section 3
    // removed the preparation screen from every path — a voice call dials
    // immediately, a video call shows the camera check before the call exists
    // ([CameraCheckScreen], presented by the overlay, not by this screen), and
    // "accepting an incoming call never shows any preparation screen". So the
    // only thing this composable draws is the call.
    LakkoTheme(dark = true) {
        InCallView(
            state = callState,
            onExit = onExit,
            remoteName = remoteName,
            avatarSeed = avatarSeed,
            groupCall = groupCall,
            groupName = groupName,
            speakerOn = speakerOn,
            onToggleSpeaker = { audio.toggleSpeaker() },
            onSwitchCamera = onSwitchCamera,
            // G3-2-session-bind / G3-2-call-controls — route the
            // end-call + mic/camera toggles to the real session
            // when one drives the screen; mock-state fallback
            // otherwise.
            onEndCall = {
                when {
                    session != null -> session.hangUp()
                    onEndCall != null -> onEndCall()
                    else -> callState.endCall()
                }
            },
            onSetMic = { on -> if (session != null) session.setMicrophoneEnabled(on) else callState.setMic(on) },
            // G3-2-call-controls + ADR-0032 piece B2b — camera routing. With a
            // real session, turning the camera ON runs `addLocalVideo()`
            // (acquire + add a video track + renegotiate the first time on a
            // call started audio-only; re-enable an existing sender after) and
            // turning it OFF is the local-only `setCameraEnabled(false)` gate.
            // A capture failure leaves `session.cameraEnabled` false, so the
            // driven store auto-reverts the button; we log the reason and the
            // call stays audio-only (fail-safe). Mock-state fallback otherwise.
            onSetCamera = { on ->
                if (session != null) {
                    if (on) {
                        cameraScope.launch {
                            try {
                                session.addLocalVideo()
                            } catch (cause: Throwable) {
                                android.util.Log.w("CallScreen", "addLocalVideo failed", cause)
                            }
                        }
                    } else {
                        // docs/ux/calls.md section 4.1 — turning the camera off
                        // has to reach the other screen too, or they keep the
                        // last frame. `stopSendingVideo` parks the transceiver
                        // at `recvOnly` and renegotiates; the sender and its
                        // SFrame encryptor stay put, so the next camera-on is
                        // the cheap re-enable branch.
                        cameraScope.launch {
                            try {
                                session.stopSendingVideo()
                            } catch (cause: Throwable) {
                                android.util.Log.w("CallScreen", "stopSendingVideo failed", cause)
                            }
                        }
                    }
                } else {
                    callState.setCamera(on)
                }
            },
        )
    }
}

// ============================================================
// InCallView
// ============================================================

@Composable
@Suppress("LongMethod", "LongParameterList")
private fun InCallView(
    state: CallUiState,
    onExit: () -> Unit,
    remoteName: String? = null,
    avatarSeed: String? = null,
    groupCall: Boolean = false,
    groupName: String? = null,
    speakerOn: Boolean = false,
    onToggleSpeaker: () -> Unit = {},
    onSwitchCamera: (() -> Unit)? = null,
    onEndCall: () -> Unit = { state.endCall() },
    onSetMic: (Boolean) -> Unit = { state.setMic(it) },
    onSetCamera: (Boolean) -> Unit = { state.setCamera(it) },
) {
    val participants by state.participants.collectAsState()
    val phase by state.phase.collectAsState()
    val drawerOpen by state.participantListOpen.collectAsState()
    val e2ee by state.e2ee.collectAsState()
    val localVideoTrack by state.localVideoTrack.collectAsState()
    val remoteVideoTrack by state.remoteVideoTrack.collectAsState()
    val videoPublishState by state.videoPublishState.collectAsState()
    val hasMultipleCameras by state.hasMultipleCameras.collectAsState()
    val settings by state.settings.collectAsState()
    val frontFacing by state.cameraFrontFacing.collectAsState()
    // docs/ux/calls.md section 5 — the digits are behind the chip, never on the
    // call. Closed on every new call because the state object is reset with the
    // screen, so a sheet can never open over a call it does not belong to.
    var encryptionSheetOpen by remember { mutableStateOf(false) }
    var moreOpen by remember { mutableStateOf(false) }

    val local = participants.firstOrNull { it.isLocal }
    val remotes = remember(participants) { participants.filter { !it.isLocal } }
    val soleRemote = remotes.singleOrNull()
    val connected = phase is CallUiState.CallPhase.Connected
    // docs/ux/calls.md section 4 — a portrait, a full-screen remote picture, or
    // the group grid. The tiled column used to be all three.
    // `docs/ux/calls.md` section 4.1 — the layout follows the cameras, not the
    // kind of call that was placed: starting the camera mid-call "turns the
    // call into a video one", and turning it off "returns the call to the
    // portrait".
    val localVideoPresent = local?.camOn == true && localVideoTrack != null
    val stage = callStage(
        remoteParticipants = remotes.size,
        remoteVideoPresent = soleRemote?.camOn == true && remoteVideoTrack != null,
        localVideoPresent = localVideoPresent,
        groupCall = groupCall,
    )
    val group = stage == CallStage.GRID
    // docs/ux/calls.md section 4 — a group call is titled by the group, never
    // by whatever the first participant's tile happens to say. See
    // [callScreenTitle]; a live call was titled `01a0a985` because the title
    // inherited a participant label's fallback chain.
    val displayName = callScreenTitle(
        groupName = groupName,
        peerName = remoteName ?: soleRemote?.displayName,
        groupCall = groupCall,
    )
    val seed = avatarSeed?.takeIf { it.isNotBlank() } ?: displayName

    // Section 4: "The controls fade out after 5 seconds of a connected video
    // call and come back on a tap; on a voice call they never hide."
    var controlsVisible by remember { mutableStateOf(true) }
    val mayHide = controlsMayAutoHide(stage, connected)
    LaunchedEffect(mayHide, controlsVisible) {
        if (!mayHide) {
            controlsVisible = true
        } else if (controlsVisible) {
            delay(CALL_CONTROLS_AUTO_HIDE_MS)
            controlsVisible = false
        }
    }

    // Section 4.1's state table: "Ended — the screen closes itself after 2 s".
    LaunchedEffect(phase) {
        if (phase is CallUiState.CallPhase.Ended) {
            delay(2_000)
            onExit()
        }
    }

    val controls = callControlRow(
        group = group,
        micOn = local?.micOn != false,
        cameraOn = local?.camOn == true,
        speakerOn = speakerOn,
        cameraBusy = videoPublishState == CallUiState.VideoPublishState.Starting,
        ringing = phase is CallUiState.CallPhase.Calling,
    )

    var rootSize by remember { mutableStateOf(IntSize.Zero) }
    val qualityPopoverOpen by state.qualityPopoverOpen.collectAsState()

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(LakkoDark.background)
            .onSizeChanged { rootSize = it },
    ) {
        when (stage) {
            CallStage.PORTRAIT -> CallPortrait(
                state = state,
                phase = phase,
                name = displayName,
                seed = seed,
                e2ee = e2ee,
                onEncryptionChip = { encryptionSheetOpen = true },
            )
            CallStage.REMOTE_VIDEO, CallStage.LOCAL_VIDEO -> {
                val egl = remember { WebRtcPeerConnection.sharedEglContext() }
                // On LOCAL_VIDEO the only picture is this device's own, and it
                // takes the main surface rather than hovering in a corner over
                // an empty portrait.
                val onLocal = stage == CallStage.LOCAL_VIDEO
                val track = if (onLocal) localVideoTrack else remoteVideoTrack
                if (track != null && egl != null) {
                    LocalVideoPreviewView(
                        track = track,
                        eglContext = egl,
                        modifier = Modifier.fillMaxSize(),
                        // Never mirror the other person: the viewer wants their
                        // true orientation. Own video is mirrored, like a mirror.
                        mirrored = onLocal && settings.mirrorLocal && frontFacing,
                        scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
                    )
                }
                FloatingStatusHeader(
                    state = state,
                    phase = phase,
                    name = displayName,
                    e2ee = e2ee,
                    onEncryptionChip = { encryptionSheetOpen = true },
                    visible = controlsVisible,
                    modifier = Modifier.align(Alignment.TopCenter),
                )
            }
            CallStage.GRID -> {
                ParticipantGrid(
                    state = state,
                    participants = participants,
                    modifier = Modifier.fillMaxSize(),
                )
                FloatingStatusHeader(
                    state = state,
                    phase = phase,
                    name = displayName,
                    e2ee = e2ee,
                    onEncryptionChip = { encryptionSheetOpen = true },
                    visible = true,
                    modifier = Modifier.align(Alignment.TopCenter),
                )
            }
        }

        // A tap "puts things back": it brings the faded controls up and closes
        // the quality popover. Mounted only while one of those is true, because
        // a permanently clickable full-screen Box is an unlabelled tap target
        // that TalkBack announces over the whole call.
        if (!controlsVisible || qualityPopoverOpen) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .clickable(
                        indication = null,
                        interactionSource = remember { MutableInteractionSource() },
                    ) {
                        controlsVisible = true
                        state.setQualityPopoverOpen(false)
                    },
            )
        }

        // Section 4 — "the self-view is a small rounded rectangle in a corner,
        // draggable, about a quarter of the width", and section 4.1's rule for
        // who gets one: only when somebody else's picture already owns the main
        // surface. [showsSelfViewPip] carries both halves.
        if (showsSelfViewPip(stage, localVideoPresent) ||
            (local?.camOn == true && videoPublishState == CallUiState.VideoPublishState.Starting)
        ) {
            SelfViewPip(
                track = localVideoTrack,
                mirrored = settings.mirrorLocal && frontFacing,
                starting = videoPublishState == CallUiState.VideoPublishState.Starting,
                onSwitchCamera = onSwitchCamera.takeIf { hasMultipleCameras },
                parentSize = rootSize,
                modifier = Modifier.align(Alignment.TopEnd),
            )
        }

        // Section 4.1 — flip camera "lives on the self-view itself". When this
        // device's own picture IS the whole screen there is no rectangle to put
        // it in, so it sits in the same corner of the screen instead.
        if (stage == CallStage.LOCAL_VIDEO && hasMultipleCameras && onSwitchCamera != null) {
            SwitchCameraButton(
                onClick = onSwitchCamera,
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .windowInsetsPadding(WindowInsets.safeDrawing)
                    .padding(top = 96.dp, end = 20.dp),
            )
        }

        // Section 4.1 — one rounded translucent bar, floating above the bottom
        // safe area with a margin. On a voice call it sits in the lower third
        // under the portrait rather than welded to the bottom edge, which is
        // what the extra padding buys.
        AnimatedVisibility(
            visible = controlsVisible,
            enter = fadeIn(),
            exit = fadeOut(),
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(
                    horizontal = 12.dp,
                    vertical = if (stage == CallStage.PORTRAIT) 72.dp else 16.dp,
                ),
        ) {
            Box {
                CallControlBar(
                    controls = controls,
                    onControl = { kind ->
                        when (kind) {
                            CallControlKind.SPEAKER -> onToggleSpeaker()
                            CallControlKind.MICROPHONE -> onSetMic(local?.micOn != true)
                            CallControlKind.CAMERA -> onSetCamera(local?.camOn != true)
                            CallControlKind.MORE -> moreOpen = true
                            CallControlKind.END -> onEndCall()
                        }
                    },
                )
                // Section 4.1 keeps participants and chat behind one overflow
                // button — in a GROUP call only. A 1x1 call has no MORE
                // control, so this menu is unreachable there.
                CallMoreMenu(state = state, expanded = moreOpen, onDismiss = { moreOpen = false })
            }
        }

        // Right-edge drawer (group calls).
        if (drawerOpen) {
            Box(
                modifier = Modifier
                    .align(Alignment.CenterEnd)
                    .fillMaxHeight()
                    .width(320.dp),
            ) {
                ParticipantListDrawer(state)
            }
        }

        // There is no ended overlay. Section 4.1 gives the ended state one
        // line — "`Call ended` and the duration" — and the status line already
        // carries it; a dimming panel repeating the same two words is the
        // floating pill the redesign removed, wearing a different shape.

        // Gear-menu bottom sheet (S-3-A-settings).
        SettingsSheet(state)
        // Chat bottom sheet (S-3-A-chat).
        ChatSheet(state)
        // docs/ux/calls.md section 5 — the safety number, only when asked for.
        if (encryptionSheetOpen) {
            CallEncryptionSheet(
                safetyNumber = e2ee.shortFingerprint,
                otherName = remoteName,
                onDismiss = { encryptionSheetOpen = false },
            )
        }
    }
}

/**
 * The voice call — `docs/ux/calls.md` section 4: "the other person's avatar
 * large and centred in the upper half, their name under it in the largest type
 * on the screen, and under that one quiet line with the state or the running
 * time. Nothing else competes with it."
 *
 * The background is section 4.2's: the contact's colour rather than a flat
 * surface, which "reads as an empty page, which is what made ours look
 * unfinished". Lakko has no contact photos to blur, so the honest form of "the
 * avatar, heavily blurred and darkened" is the named fallback — the initials
 * colour, spread as a soft radial wash that needs no blur API and therefore
 * looks the same on every supported Android version.
 */
@Composable
private fun CallPortrait(
    state: CallUiState,
    phase: CallUiState.CallPhase,
    name: String,
    seed: String,
    e2ee: CallUiState.E2eeState,
    onEncryptionChip: () -> Unit,
) {
    val tint = remember(seed) { avatarTintFor(seed, dark = true) }
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(
                Brush.radialGradient(
                    colors = listOf(
                        tint.background.copy(alpha = 0.9f),
                        LakkoDark.background,
                    ),
                ),
            ),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(horizontal = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            // The upper half: the spec's placement, not a centred block.
            Spacer(Modifier.weight(0.18f))
            LakkoAvatar(name = name, seed = seed, size = 132.dp)
            Spacer(Modifier.height(24.dp))
            CallStatusLine(
                state = state,
                phase = phase,
                remoteName = name,
                e2ee = e2ee,
                onEncryptionChip = onEncryptionChip,
                portrait = true,
            )
            Spacer(Modifier.weight(0.82f))
        }
    }
}

/**
 * The status line over a picture — `docs/ux/calls.md` section 4: "The name and
 * the state float over the video at the top, on a soft shade so they stay
 * readable."
 *
 * It fades with the controls, for the same reason they fade: on a video call
 * the thing worth looking at is underneath.
 */
@Composable
private fun FloatingStatusHeader(
    state: CallUiState,
    phase: CallUiState.CallPhase,
    name: String,
    e2ee: CallUiState.E2eeState,
    onEncryptionChip: () -> Unit,
    visible: Boolean,
    modifier: Modifier = Modifier,
) {
    AnimatedVisibility(visible = visible, enter = fadeIn(), exit = fadeOut(), modifier = modifier) {
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .background(
                    Brush.verticalGradient(
                        listOf(LakkoDark.background.copy(alpha = 0.72f), Color.Transparent),
                    ),
                )
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(horizontal = 20.dp, vertical = 12.dp),
        ) {
            CallStatusLine(
                state = state,
                phase = phase,
                remoteName = name,
                e2ee = e2ee,
                onEncryptionChip = onEncryptionChip,
                portrait = false,
            )
        }
    }
}

/**
 * The self-view of `docs/ux/calls.md` section 4: a small rounded rectangle in a
 * corner, draggable, about a quarter of the width, carrying the flip-camera
 * button of section 4.1 "where the thing it changes is visible".
 */
@Composable
private fun SelfViewPip(
    track: VideoTrack?,
    mirrored: Boolean,
    starting: Boolean,
    onSwitchCamera: (() -> Unit)?,
    parentSize: IntSize,
    modifier: Modifier = Modifier,
) {
    val density = LocalDensity.current
    var offset by remember { mutableStateOf(Offset.Zero) }
    val widthDp = 104.dp
    val heightDp = 152.dp
    // Clamped so a drag can never park the picture off the screen; the start
    // corner is the top right, under the status header.
    val (maxX, maxY) = with(density) {
        val w = parentSize.width - widthDp.toPx()
        val h = parentSize.height - heightDp.toPx()
        (if (w > 0f) w else 0f) to (if (h > 0f) h else 0f)
    }

    Box(
        modifier = modifier
            .padding(top = 110.dp, end = 16.dp)
            .offset { IntOffset(offset.x.roundToInt(), offset.y.roundToInt()) }
            .size(width = widthDp, height = heightDp)
            .clip(RoundedCornerShape(16.dp))
            .background(LakkoDark.sunken)
            .pointerInput(maxX, maxY) {
                detectDragGestures { change, delta ->
                    change.consume()
                    offset = Offset(
                        x = (offset.x + delta.x).coerceIn(-maxX, 0f),
                        y = (offset.y + delta.y).coerceIn(0f, maxY),
                    )
                }
            },
    ) {
        val egl = remember { WebRtcPeerConnection.sharedEglContext() }
        if (track != null && egl != null) {
            LocalVideoPreviewView(
                track = track,
                eglContext = egl,
                modifier = Modifier.fillMaxSize(),
                mirrored = mirrored,
                scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
            )
        } else if (starting) {
            // ADR-0032 piece B2a — the acquire is in flight, so the rectangle
            // says so instead of showing an empty box.
            Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator(
                    modifier = Modifier.size(20.dp),
                    color = LakkoDark.textMuted,
                    strokeWidth = 2.dp,
                )
            }
        }
        if (onSwitchCamera != null) {
            SwitchCameraButton(
                onClick = onSwitchCamera,
                modifier = Modifier.align(Alignment.BottomEnd).padding(6.dp),
            )
        }
    }
}

/**
 * Flip to the other camera — `docs/ux/calls.md` section 4.1, which keeps it out
 * of the control row and puts it "on the self-view itself ... where the thing
 * it changes is visible". Before the redesign this was a dropdown entry with an
 * empty body, so the one control a person reaches for mid-sentence was two taps
 * away and did nothing.
 */
@Composable
private fun SwitchCameraButton(onClick: () -> Unit, modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .size(36.dp)
            .clip(CircleShape)
            .background(LakkoDark.background.copy(alpha = 0.6f))
            .clickable(onClick = onClick)
            .semantics { contentDescription = "Switch camera"; role = Role.Button },
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            Icons.Filled.FlipCameraAndroid,
            contentDescription = null,
            tint = LakkoDark.text,
            modifier = Modifier.size(18.dp),
        )
    }
}

/**
 * The group call's roster of tiles — unchanged in shape, because section 9 puts
 * "group call redesign beyond the shared call screen" out of scope. It now sits
 * behind the floating header and control bar rather than between them, so the
 * padding leaves room for both.
 */
@Composable
private fun ParticipantGrid(
    state: CallUiState,
    participants: List<CallUiState.Participant>,
    modifier: Modifier = Modifier,
) {
    val phase by state.phase.collectAsState()
    Box(modifier = modifier) {
        // Derived from the COLLECTED list. Reading `state.participants.value`
        // inside a lazy list's content lambda is not observed, so the tiles
        // froze at whatever the roster was on first composition (an empty
        // screen when the overlay opened before the SFU welcome arrived).
        val active = remember(participants) { state.activeSpeaker() }
        val gallery = remember(participants, active) {
            participants.filter { it.id != active?.id }
        }
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(start = 16.dp, end = 16.dp, top = 76.dp, bottom = 140.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (active != null) {
                ParticipantTile(participant = active, large = true, state = state)
            }
            gallery.chunked(2).forEach { row ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    row.forEach { p ->
                        key(p.id) {
                            Box(modifier = Modifier.weight(1f)) {
                                ParticipantTile(participant = p, large = false, state = state)
                            }
                        }
                    }
                    // Keep a lone gallery tile half width, like the grid did.
                    if (row.size == 1) {
                        Spacer(modifier = Modifier.weight(1f))
                    }
                }
            }
        }

        // Waiting overlay when alone.
        if (state.isAlone() && phase is CallUiState.CallPhase.Connected) {
            Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Box(
                    modifier = Modifier
                        .background(LakkoDark.surface.copy(alpha = 0.9f), RoundedCornerShape(12.dp))
                        .border(1.dp, LakkoDark.border, RoundedCornerShape(12.dp))
                        .padding(horizontal = 24.dp, vertical = 16.dp),
                ) {
                    Text("Waiting for others to join...", color = LakkoDark.text, fontSize = 14.sp)
                }
            }
        }
    }
}

// ============================================================
// SettingsSheet (S-3-A-settings)
// ============================================================
//
// Material3 ModalBottomSheet that appears when the user taps
// the gear button in the controls bar. Mirrors the same control
// surface as the web `SettingsPanel` and iOS `SettingsSheet`.

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SettingsSheet(state: CallUiState) {
    val open by state.settingsOpen.collectAsState()
    if (!open) return

    val settings by state.settings.collectAsState()
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)

    ModalBottomSheet(
        onDismissRequest = { state.setSettingsOpen(false) },
        sheetState = sheetState,
        containerColor = LakkoDark.surface,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp, vertical = 8.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text(
                "Settings",
                color = LakkoDark.text,
                fontSize = 18.sp,
                fontWeight = FontWeight.SemiBold,
            )

            DeviceSectionLabel("Devices")
            DevicePickerRow(
                label = "Microphone",
                devices = settings.availableDevices.filter {
                    it.kind == CallUiState.CallDevice.Kind.AudioInput
                },
                selectedId = settings.selectedAudioInput,
                onPick = { state.setSelectedAudioInput(it) },
            )
            DevicePickerRow(
                label = "Camera",
                devices = settings.availableDevices.filter {
                    it.kind == CallUiState.CallDevice.Kind.VideoInput
                },
                selectedId = settings.selectedVideoInput,
                onPick = { state.setSelectedVideoInput(it) },
            )
            DevicePickerRow(
                label = "Speaker",
                devices = settings.availableDevices.filter {
                    it.kind == CallUiState.CallDevice.Kind.AudioOutput
                },
                selectedId = settings.selectedAudioOutput,
                onPick = { state.setSelectedAudioOutput(it) },
            )

            Divider(color = LakkoDark.border, modifier = Modifier.padding(vertical = 4.dp))

            DeviceSectionLabel("Audio processing")
            SettingsToggleRow(
                label = "Noise suppression",
                checked = settings.noiseSuppression,
                onChange = { state.setNoiseSuppression(it) },
            )
            SettingsToggleRow(
                label = "Echo cancellation",
                checked = settings.echoCancellation,
                onChange = { state.setEchoCancellation(it) },
            )
            SettingsToggleRow(
                label = "Automatic gain control",
                checked = settings.autoGain,
                onChange = { state.setAutoGain(it) },
            )

            DeviceSectionLabel("Video")
            SettingsToggleRow(
                label = "Mirror local preview",
                checked = settings.mirrorLocal,
                onChange = { state.setMirrorLocal(it) },
            )

            DeviceSectionLabel("Network")
            SettingsToggleRow(
                label = "Low-data mode",
                description = "Caps video at 360p — useful on metered mobile connections.",
                checked = settings.lowDataMode,
                onChange = { state.setLowDataMode(it) },
            )
        }
    }
}

@Composable
private fun DeviceSectionLabel(text: String) {
    Text(
        text,
        color = LakkoDark.textMuted,
        fontSize = 12.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier.padding(top = 8.dp),
    )
}

@Composable
private fun DevicePickerRow(
    label: String,
    devices: List<CallUiState.CallDevice>,
    selectedId: String?,
    onPick: (String?) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val selectedLabel = devices.firstOrNull { it.id == selectedId }?.label
        ?: "System default"

    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(label, color = LakkoDark.text, fontSize = 13.sp)
        Box {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(LakkoDark.background, RoundedCornerShape(8.dp))
                    .border(1.dp, LakkoDark.border, RoundedCornerShape(8.dp))
                    .clickable { expanded = true }
                    .padding(horizontal = 10.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    selectedLabel,
                    color = LakkoDark.text,
                    fontSize = 14.sp,
                    modifier = Modifier.weight(1f),
                )
                Icon(
                    Icons.Filled.ArrowDropDown,
                    contentDescription = null,
                    tint = LakkoDark.textMuted,
                )
            }
            DropdownMenu(
                expanded = expanded,
                onDismissRequest = { expanded = false },
            ) {
                DropdownMenuItem(
                    text = { Text("System default") },
                    onClick = {
                        onPick(null)
                        expanded = false
                    },
                )
                for (d in devices) {
                    DropdownMenuItem(
                        text = { Text(d.label.ifEmpty { d.kind.name }) },
                        onClick = {
                            onPick(d.id)
                            expanded = false
                        },
                    )
                }
            }
        }
        if (devices.isEmpty()) {
            Text(
                "No devices reported yet. They appear here once you grant " +
                    "microphone or camera permission.",
                color = LakkoDark.textMuted,
                fontSize = 11.sp,
            )
        }
    }
}

@Composable
private fun SettingsToggleRow(
    label: String,
    description: String? = null,
    checked: Boolean,
    onChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable { onChange(!checked) }
            .padding(vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(label, color = LakkoDark.text, fontSize = 14.sp)
            if (description != null) {
                Text(description, color = LakkoDark.textMuted, fontSize = 11.sp)
            }
        }
        Switch(checked = checked, onCheckedChange = onChange)
    }
}

// ============================================================
// ChatSheet (S-3-A-chat)
// ============================================================

/** Bottom-sheet chat panel. Text-only; system events
 *  ("Alice joined") rendered as grey italic centred lines.
 *  Mock transport — messages live in CallUiState only until
 *  the WebRTC pod wires real MLS application-message
 *  round-trips. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChatSheet(state: CallUiState) {
    val open by state.chatOpen.collectAsState()
    if (!open) return

    val messages by state.chatMessages.collectAsState()
    val compose by state.composeText.collectAsState()
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val listState = rememberLazyListState()

    // Auto-scroll to bottom on new message.
    LaunchedEffect(messages.size) {
        if (messages.isNotEmpty()) {
            listState.animateScrollToItem(messages.size - 1)
        }
    }

    ModalBottomSheet(
        onDismissRequest = { state.setChatOpen(false) },
        sheetState = sheetState,
        containerColor = LakkoDark.surface,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 360.dp)
                .padding(horizontal = 16.dp)
                .padding(bottom = 12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                "Chat",
                color = LakkoDark.text,
                fontSize = 18.sp,
                fontWeight = FontWeight.SemiBold,
            )

            if (messages.isEmpty()) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        "No messages yet. Messages here are end-to-end encrypted.",
                        color = LakkoDark.textMuted,
                        fontSize = 13.sp,
                        textAlign = TextAlign.Center,
                    )
                }
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f),
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    items(messages, key = { it.id }) { msg ->
                        ChatMessageRow(msg)
                    }
                }
            }

            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                OutlinedTextField(
                    value = compose,
                    onValueChange = { state.setComposeText(it) },
                    modifier = Modifier.weight(1f),
                    placeholder = { Text("Send a message") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                    keyboardActions = KeyboardActions(
                        onSend = { state.sendChatMessage(compose) }
                    ),
                )
                Button(
                    onClick = { state.sendChatMessage(compose) },
                    enabled = compose.trim().isNotEmpty(),
                ) {
                    Text("Send")
                }
            }
        }
    }
}

@Composable
private fun ChatMessageRow(msg: CallUiState.ChatMessage) {
    if (msg.kind == CallUiState.ChatMessage.Kind.System) {
        Text(
            msg.text,
            color = LakkoDark.textMuted,
            fontSize = 12.sp,
            fontStyle = FontStyle.Italic,
            textAlign = TextAlign.Center,
            modifier = Modifier.fillMaxWidth(),
        )
    } else {
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(
                verticalAlignment = Alignment.Bottom,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    msg.senderName,
                    color = LakkoDark.accent,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    formatChatTimestamp(msg.tsMs),
                    color = LakkoDark.textMuted,
                    fontSize = 12.sp,
                )
            }
            Text(
                msg.text,
                color = LakkoDark.text,
                fontSize = 14.sp,
            )
        }
    }
}

private fun formatChatTimestamp(epochMs: Long): String {
    val cal = java.util.Calendar.getInstance().apply { timeInMillis = epochMs }
    val hh = cal.get(java.util.Calendar.HOUR_OF_DAY).toString().padStart(2, '0')
    val mm = cal.get(java.util.Calendar.MINUTE).toString().padStart(2, '0')
    return "$hh:$mm"
}

// ============================================================
// Subcomponents
// ============================================================

/**
 * `docs/ux/calls.md` section 4 item 1 — who the call is with, and what it is
 * doing. The running time ticks from the moment this screen observes the call
 * connect; before that there is a state word and never a zeroed clock.
 */
@Composable
@Suppress("LongParameterList")
private fun CallStatusLine(
    state: CallUiState,
    phase: CallUiState.CallPhase,
    remoteName: String?,
    e2ee: CallUiState.E2eeState,
    onEncryptionChip: () -> Unit,
    /** The portrait of section 4 puts the name "in the largest type on the
     *  screen"; over a video it is one readable line among others. */
    portrait: Boolean,
    modifier: Modifier = Modifier,
) {
    val connected = phase is CallUiState.CallPhase.Connected
    val ended = phase is CallUiState.CallPhase.Ended
    // Wall clock at the transition, then a one-second tick. The tick STOPS when
    // the call leaves `Connected` — a finished call must not keep counting
    // while it waits to dismiss itself — but the last value is KEPT once the
    // call has ended, because section 4.1 asks the ended state for the
    // duration and there is nowhere else to get it from.
    var elapsedSecs by remember { mutableStateOf<Long?>(null) }
    LaunchedEffect(connected, ended) {
        if (!connected) {
            if (!ended) elapsedSecs = null
            return@LaunchedEffect
        }
        val startedAt = System.currentTimeMillis()
        while (true) {
            elapsedSecs = ((System.currentTimeMillis() - startedAt) / 1_000L).coerceAtLeast(0L)
            delay(1_000)
        }
    }

    // Already decided by [callScreenTitle] at the stage; passed through whole so
    // the line cannot re-derive a different answer from the roster.
    val title = remoteName?.trim()?.takeIf { it.isNotEmpty() } ?: "Call"
    val status = callStatusText(
        calling = phase is CallUiState.CallPhase.Calling,
        connecting = phase is CallUiState.CallPhase.Connecting,
        connected = connected,
        reconnecting = phase is CallUiState.CallPhase.Reconnecting,
        ended = phase is CallUiState.CallPhase.Ended,
        elapsedSecs = elapsedSecs,
    )
    val spoken = callStatusSpokenLabel(title, status, elapsedSecs)
    Column(
        modifier = modifier,
        horizontalAlignment = if (portrait) Alignment.CenterHorizontally else Alignment.Start,
    ) {
        Text(
            text = title,
            color = LakkoDark.text,
            // Section 4: on a voice call the name is the largest type on the
            // screen; nothing else competes with it.
            fontSize = if (portrait) 32.sp else 17.sp,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.clearAndSetSemantics { },
        )
        Spacer(Modifier.height(if (portrait) 8.dp else 2.dp))
        // Section 4: one status line, and the lock chip BESIDE it — never a
        // second floating pill repeating the same word somewhere else.
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.semantics(mergeDescendants = true) { contentDescription = spoken },
        ) {
            Text(
                text = status,
                color = LakkoDark.textMuted,
                fontSize = if (portrait) 15.sp else 13.sp,
                maxLines = 1,
            )
            if (showsCallEncryptionChip(e2ee.active, e2ee.shortFingerprint)) {
                CallEncryptionChip(onClick = onEncryptionChip)
            }
            QualityIndicator(state)
        }
    }
}

/**
 * `docs/ux/calls.md` sections 4 and 5 — one small lock chip on a sunken surface,
 * reading `Encrypted`. It replaced the permanent strip of sixteen digits; the
 * digits now live in [CallEncryptionSheet], behind a tap.
 *
 * Rendered only when the call's encryption is actually armed, so its presence is
 * the signal. There is deliberately no "pending" or "unavailable" variant: a
 * call that cannot arm its key refuses to start (security invariant #4) rather
 * than running with a lock the user would have to interpret.
 */
@Composable
private fun CallEncryptionChip(onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .clip(RoundedCornerShape(50))
            .background(LakkoDark.sunken, RoundedCornerShape(50))
            .border(1.dp, LakkoDark.border, RoundedCornerShape(50))
            .clickable(onClick = onClick)
            .semantics(mergeDescendants = true) {
                contentDescription = "Encrypted call. Double tap to see the safety number."
            }
            .padding(horizontal = 10.dp, vertical = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(5.dp),
    ) {
        Icon(
            Icons.Filled.Lock,
            contentDescription = null,
            tint = LakkoDark.success,
            modifier = Modifier.size(12.dp),
        )
        Text(
            text = CALL_ENCRYPTION_CHIP_LABEL,
            color = LakkoDark.text,
            fontSize = 12.sp,
            fontWeight = FontWeight.Medium,
        )
    }
}

/**
 * `docs/ux/calls.md` section 5 — the sheet behind the lock chip: what the call
 * is, the safety number in four groups of four, what to do with it, Copy and
 * Done. This is the only place the digits appear.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CallEncryptionSheet(
    safetyNumber: String?,
    otherName: String?,
    onDismiss: () -> Unit,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val clipboard = LocalClipboardManager.current
    val groups = remember(safetyNumber) { safetyNumberGroups(safetyNumber) }
    val spoken = remember(safetyNumber) { safetyNumberSpokenLabel(safetyNumber) }
    var copied by remember { mutableStateOf(false) }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = LakkoDark.surface,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 24.dp)
                .padding(bottom = 32.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(
                CALL_ENCRYPTION_SHEET_TITLE,
                color = LakkoDark.text,
                fontSize = 20.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                callEncryptionSentence(otherName),
                color = LakkoDark.textMuted,
                fontSize = 14.sp,
            )
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(LakkoDark.sunken, RoundedCornerShape(12.dp))
                    .padding(vertical = 18.dp)
                    .semantics(mergeDescendants = true) { contentDescription = spoken },
                horizontalArrangement = Arrangement.spacedBy(14.dp, Alignment.CenterHorizontally),
            ) {
                groups.forEach { group ->
                    Text(
                        group,
                        color = LakkoDark.text,
                        fontSize = 22.sp,
                        fontWeight = FontWeight.SemiBold,
                        fontFamily = FontFamily.Monospace,
                    )
                }
            }
            Text(
                CALL_ENCRYPTION_SHEET_HINT,
                color = LakkoDark.textMuted,
                fontSize = 13.sp,
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                OutlinedButton(
                    onClick = {
                        clipboard.setText(AnnotatedString(groups.joinToString(" ")))
                        copied = true
                    },
                    modifier = Modifier.weight(1f),
                    shape = RoundedCornerShape(8.dp),
                ) {
                    Text(if (copied) "Copied" else "Copy", color = LakkoDark.text)
                }
                Button(
                    onClick = onDismiss,
                    modifier = Modifier.weight(1f),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = LakkoDark.accent,
                        contentColor = LakkoDark.accentFg,
                    ),
                    shape = RoundedCornerShape(8.dp),
                ) {
                    Text("Done")
                }
            }
        }
    }
}

@Composable
private fun QualityIndicator(state: CallUiState) {
    val tier by remember(state) { mutableStateOf(state.localQualityTier()) }.let {
        // collect connectionDetail + participants so recompositions track changes
        val _participants_dep by state.participants.collectAsState()
        val _detail_dep by state.connectionDetail.collectAsState()
        @Suppress("UNUSED_VARIABLE") val _local_dep = _participants_dep
        @Suppress("UNUSED_VARIABLE") val _conn_dep = _detail_dep
        remember(state.localQualityTier()) { mutableStateOf(state.localQualityTier()) }
    }
    val popoverOpen by state.qualityPopoverOpen.collectAsState()
    val detail by state.connectionDetail.collectAsState()

    Box {
        Box(
            modifier = Modifier
                .size(8.dp)
                .background(tier.colour(), CircleShape)
                .shadow(3.dp, CircleShape)
                .clickable { state.setQualityPopoverOpen(!popoverOpen) },
        )
        if (popoverOpen) {
            Column(
                modifier = Modifier
                    .widthIn(min = 180.dp)
                    .wrapContentSize(align = Alignment.TopEnd)
                    .background(LakkoDark.surface, RoundedCornerShape(8.dp))
                    .border(1.dp, LakkoDark.border, RoundedCornerShape(8.dp))
                    .padding(10.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(
                        tier.name,
                        color = LakkoDark.text,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        when (detail.connectionType) {
                            CallUiState.ConnectionDetail.ConnectionType.Direct -> "Direct"
                            CallUiState.ConnectionDetail.ConnectionType.Relay -> "Relay"
                            CallUiState.ConnectionDetail.ConnectionType.Unknown -> "—"
                        },
                        color = when (detail.connectionType) {
                            CallUiState.ConnectionDetail.ConnectionType.Direct -> LakkoDark.success
                            CallUiState.ConnectionDetail.ConnectionType.Relay -> LakkoDark.warning
                            CallUiState.ConnectionDetail.ConnectionType.Unknown -> LakkoDark.textMuted
                        },
                        fontSize = 12.sp,
                    )
                }
                Divider(color = LakkoDark.border)
                Text(
                    "RTT: ${detail.rttMs?.toString()?.plus(" ms") ?: "—"}",
                    color = LakkoDark.text,
                    fontSize = 11.sp,
                )
                Text(
                    "↓ ${formatBps(detail.downBps)}   ↑ ${formatBps(detail.upBps)}",
                    color = LakkoDark.text,
                    fontSize = 11.sp,
                )
            }
        }
    }
}

private fun formatBps(bps: Int?): String {
    if (bps == null) return "—"
    if (bps >= 1_000_000) return "%.1f Mbps".format(bps / 1_000_000.0)
    if (bps >= 1_000) return "${bps / 1_000} kbps"
    return "$bps bps"
}

@Composable
private fun ParticipantTile(
    participant: CallUiState.Participant,
    large: Boolean,
    state: CallUiState,
) {
    val pinnedId by state.pinnedId.collectAsState()
    val isPinned = pinnedId == participant.id
    // G3-2-video-ui-screen — observe the live local track so a
    // post-construction camera-arm flips this tile from
    // placeholder to live preview without an outer recomposition
    // dance. G3-2-remote-video — also observe the remote track,
    // rendered for the sole remote participant (1×1). N-way
    // per-participant mapping is the deferred follow-up (gated on
    // the SFU's MSID→leaf stream-id mapping the client consumes).
    val localVideoTrack by state.localVideoTrack.collectAsState()
    val remoteVideoTrack by state.remoteVideoTrack.collectAsState()
    // N-way SFU per-source-peer video map (keyed on the roster id
    // `"peer-<streamId>"`). A group tile renders its own peer's track from
    // here; the single [remoteVideoTrack] above stays for the 1×1 P2P case.
    val remoteVideoTracks by state.remoteVideoTracks.collectAsState()
    val participants by state.participants.collectAsState()
    val settings by state.settings.collectAsState()
    val frontFacing by state.cameraFrontFacing.collectAsState()
    val soleRemoteId = remember(participants) { state.soleRemoteParticipantId }
    // ADR-0032 piece B2a — the local self-tile shows a "Starting camera..."
    // hint while the mid-call camera-on acquire is in flight.
    val videoPublishState by state.videoPublishState.collectAsState()
    val cameraStarting = participant.isLocal &&
        videoPublishState == CallUiState.VideoPublishState.Starting

    Box(
        modifier = Modifier
            .fillMaxWidth()
            .aspectRatio(if (large) 16f / 9f else 4f / 3f)
            .background(LakkoDark.surface, RoundedCornerShape(12.dp))
            .border(
                width = if (participant.speaking) 2.dp else 1.dp,
                color = if (participant.speaking) LakkoDark.accent else LakkoDark.border,
                shape = RoundedCornerShape(12.dp),
            )
            .clickable(enabled = !participant.isLocal) {
                state.pin(if (isPinned) null else participant.id)
            },
    ) {
        // Video surface.
        if (participant.camOn) {
            // Live local-track render path (G3-2-video-ui-screen).
            // The EGL context is the same share-group the codec /
            // capturer use — sharedEglContext() is non-null once
            // any WebRtcPeerConnection has primed the factory,
            // which is true by the time a local track is
            // attached. Defensive fallback to the placeholder if
            // it is unexpectedly null.
            val eglContext = remember { WebRtcPeerConnection.sharedEglContext() }
            // Pick the track for this tile: the local track for the
            // local participant, the remote track for the sole
            // remote participant (1×1). Remote video is never
            // mirrored — the viewer wants the other party's true
            // orientation.
            val track = when {
                participant.isLocal -> localVideoTrack
                // N-way SFU group path: this peer's own track from the
                // per-peer map (keyed on the roster id == "peer-<streamId>").
                // Fall back to the single-remote handle only for the 1×1
                // sole-remote case (the P2P driveFrom path has no MSID map),
                // so N-way video renders without regressing 1×1.
                else -> remoteVideoTracks[participant.id]
                    ?: if (participant.id == soleRemoteId) remoteVideoTrack else null
            }
            if (track != null && eglContext != null) {
                LocalVideoPreviewView(
                    track = track,
                    eglContext = eglContext,
                    modifier = Modifier.fillMaxSize(),
                    // Only a front camera is mirrored: a back camera shown in a
                    // mirror would put the world the wrong way round.
                    mirrored = participant.isLocal && settings.mirrorLocal && frontFacing,
                    scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
                )
            } else {
                // Surfaces are flat (design system section 1): no gradient
                // placeholder while the track has not arrived yet.
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .background(LakkoDark.sunken, RoundedCornerShape(12.dp)),
                    contentAlignment = Alignment.Center,
                ) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(24.dp),
                        color = LakkoDark.textMuted,
                        strokeWidth = 2.dp,
                    )
                }
            }
        } else {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(LakkoDark.sunken, RoundedCornerShape(12.dp)),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = participant.displayName.firstOrNull()?.uppercase() ?: "?",
                    color = LakkoDark.text,
                    fontSize = if (large) 80.sp else 40.sp,
                    fontWeight = FontWeight.SemiBold,
                )
            }
        }

        // ADR-0032 piece B2a — mid-call camera-on loading hint on the local
        // self-tile only (a spinner + "Starting camera..." while the acquire +
        // renegotiation are in flight). Clears on Live / Failed.
        if (cameraStarting) {
            Box(
                modifier = Modifier
                    .align(Alignment.Center)
                    .background(LakkoDark.background.copy(alpha = 0.55f), RoundedCornerShape(8.dp))
                    .padding(horizontal = 12.dp, vertical = 8.dp),
                contentAlignment = Alignment.Center,
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(14.dp),
                        color = LakkoDark.text,
                        strokeWidth = 2.dp,
                    )
                    Text(
                        "Starting camera...",
                        color = LakkoDark.text,
                        fontSize = 12.sp,
                    )
                }
            }
        }

        // Centre mute overlay for muted remotes.
        if (!participant.micOn && !participant.isLocal) {
            Box(
                modifier = Modifier
                    .align(Alignment.Center)
                    .size(if (large) 56.dp else 36.dp)
                    .background(LakkoDark.background.copy(alpha = 0.55f), CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Filled.MicOff,
                    contentDescription = "Muted",
                    tint = LakkoDark.danger,
                    modifier = Modifier.size(if (large) 32.dp else 20.dp),
                )
            }
        }

        // Top-left screen-share badge.
        if (participant.screenSharing) {
            Box(
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .padding(8.dp)
                    .background(LakkoDark.background.copy(alpha = 0.6f), RoundedCornerShape(6.dp))
                    .padding(horizontal = 6.dp, vertical = 2.dp),
            ) {
                Text("Sharing", color = LakkoDark.text, fontSize = 10.sp)
            }
        }

        // Top-right hand badge.
        if (participant.handRaised) {
            Box(
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .padding(8.dp)
                    .background(LakkoDark.background.copy(alpha = 0.6f), RoundedCornerShape(6.dp))
                    .padding(4.dp),
            ) {
                Icon(
                    Icons.Filled.FrontHand,
                    contentDescription = "Hand raised",
                    tint = LakkoDark.text,
                    modifier = Modifier.size(14.dp),
                )
            }
        }

        // Bottom strip: name + mute + quality + pin.
        Row(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                .background(LakkoDark.background.copy(alpha = 0.45f))
                .padding(horizontal = 10.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (!participant.micOn) {
                Icon(
                    Icons.Filled.MicOff,
                    contentDescription = "Muted",
                    tint = LakkoDark.danger,
                    modifier = Modifier.size(12.dp),
                )
            }
            Text(
                text = if (participant.displayName.isNotEmpty()) {
                    participant.displayName
                } else if (participant.isLocal) {
                    "You"
                } else {
                    "Unknown"
                },
                color = LakkoDark.text,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Box(
                modifier = Modifier
                    .size(8.dp)
                    .background(participant.quality.colour(), CircleShape),
            )
            if (isPinned) {
                Icon(
                    Icons.Filled.PushPin,
                    contentDescription = "Pinned",
                    tint = LakkoDark.accent,
                    modifier = Modifier.size(12.dp),
                )
            }
        }
    }
}

/**
 * The group call's overflow — `docs/ux/calls.md` section 4.1: "A group call
 * keeps its participants and chat entries, and those may live behind a single
 * overflow button — but mute, camera, speaker and end stay in the row."
 *
 * What used to live here and does not any more: mute, camera and end were
 * loose circles beside an ellipsis that hid everything else, and **switch
 * camera** was one of the hidden entries with an empty body, so the one thing a
 * person does mid-sentence was both two taps away and broken. It is now a real
 * button on the self-view ([SelfViewPip]).
 *
 * A 1x1 call has no MORE control at all ([callControlKinds]), so nothing here
 * is reachable from one.
 */
@Composable
private fun CallMoreMenu(state: CallUiState, expanded: Boolean, onDismiss: () -> Unit) {
    val participants by state.participants.collectAsState()
    val local = participants.firstOrNull { it.isLocal }
    val drawerOpen by state.participantListOpen.collectAsState()
    val settingsOpen by state.settingsOpen.collectAsState()
    val chatOpen by state.chatOpen.collectAsState()
    val chatUnread by state.chatUnreadCount.collectAsState()

    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss) {
        DropdownMenuItem(
            text = { Text("Participants") },
            leadingIcon = { Icon(Icons.Filled.Group, contentDescription = null) },
            onClick = {
                onDismiss()
                state.setParticipantListOpen(!drawerOpen)
            },
        )
        DropdownMenuItem(
            text = { Text(if (chatUnread > 0) "Chat ($chatUnread unread)" else "Chat") },
            leadingIcon = { Icon(Icons.Filled.Chat, contentDescription = null) },
            onClick = {
                onDismiss()
                state.setChatOpen(!chatOpen)
            },
        )
        DropdownMenuItem(
            text = { Text(if (local?.handRaised == true) "Lower hand" else "Raise hand") },
            leadingIcon = { Icon(Icons.Filled.PanTool, contentDescription = null) },
            onClick = {
                onDismiss()
                state.setHandRaised(local?.handRaised != true)
            },
        )
        DropdownMenuItem(
            text = {
                Text(if (local?.screenSharing == true) "Stop sharing screen" else "Share screen")
            },
            leadingIcon = { Icon(Icons.Filled.ScreenShare, contentDescription = null) },
            onClick = {
                onDismiss()
                state.setScreenSharing(local?.screenSharing != true)
            },
        )
        DropdownMenuItem(
            text = { Text("Settings") },
            leadingIcon = { Icon(Icons.Filled.Settings, contentDescription = null) },
            onClick = {
                onDismiss()
                state.setSettingsOpen(!settingsOpen)
            },
        )
    }
}

@Composable
private fun ParticipantListDrawer(state: CallUiState) {
    val participants by state.participants.collectAsState()

    Column(
        modifier = Modifier
            .fillMaxHeight()
            .width(320.dp)
            .background(LakkoDark.surface)
            .border(1.dp, LakkoDark.border),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                "Participants (${participants.size})",
                color = LakkoDark.text,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            IconButton(onClick = { state.setParticipantListOpen(false) }) {
                Icon(
                    Icons.Filled.Close,
                    contentDescription = "Close participant list",
                    tint = LakkoDark.textMuted,
                )
            }
        }
        Divider(color = LakkoDark.border)
        LazyColumn(modifier = Modifier.fillMaxSize()) {
            items(participants, key = { it.id }) { p ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    Box(
                        modifier = Modifier
                            .size(8.dp)
                            .background(p.quality.colour(), CircleShape),
                    )
                    Text(
                        text = if (p.displayName.isNotEmpty()) p.displayName
                               else if (p.isLocal) "You" else "Unknown",
                        color = LakkoDark.text,
                        modifier = Modifier.weight(1f),
                    )
                    if (p.handRaised) {
                        Icon(
                            Icons.Filled.FrontHand,
                            contentDescription = "Hand raised",
                            tint = LakkoDark.text,
                            modifier = Modifier.size(16.dp),
                        )
                    }
                    if (!p.micOn) {
                        Icon(
                            Icons.Filled.MicOff,
                            contentDescription = "Muted",
                            tint = LakkoDark.danger,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                    if (p.screenSharing) {
                        Icon(
                            Icons.Filled.ScreenShare,
                            contentDescription = "Sharing screen",
                            tint = LakkoDark.text,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                }
                Divider(color = LakkoDark.border)
            }
        }
    }
}
