// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
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
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import androidx.compose.ui.platform.LocalContext
import me.lakko.app.calls.CallSession
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.ui.theme.LakkoTheme
import androidx.compose.material.icons.filled.FrontHand
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import me.lakko.app.calls.CallSoundController
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.LocalVideoPreviewView
import me.lakko.app.calls.WebRtcPeerConnection
import org.webrtc.RendererCommon

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
    state: CallUiState = remember { CallUiState() },
    // G3-2-session-bind — the live call session driving this
    // screen. When `null` (Compose previews / design work) the
    // screen runs on `seedMockState()` exactly as before. When
    // injected, `driveFrom` wires phase + roster + video to the
    // real call and the end-call button routes to
    // `session.hangUp()`.
    session: CallSession? = null,
    // G3-2-prejoin-join — when the screen is driven by a real
    // session, Join dials it (with the prejoin mic / camera
    // choice) and Cancel abandons the prepped-but-undialed call.
    // `null` on the mock design path (Join seeds mock state).
    onPrejoinJoin: ((micOn: Boolean, camOn: Boolean) -> Unit)? = null,
    onPrejoinCancel: (() -> Unit)? = null,
    /** ADR-0048 — the remote participant's resolved name for a 1x1 session. */
    remoteName: String? = null,
    /** The End button for a call the app-level host drives through [state]
     *  alone (a group or call-link call): the host leaves the call and the
     *  session's own closed transition shows the ended state. `null` keeps the
     *  session hang-up (1x1) or, with no session, the mock end. */
    onEndCall: (() -> Unit)? = null,
) {
    // One sound controller per CallScreen lifetime; released
    // in the same DisposableEffect that resets the UI state so
    // the SoundPool + cached MediaPlayer handles do not leak
    // their AudioTrack resources when the user navigates away.
    val context = LocalContext.current
    val sounds = remember(context) { CallSoundController(context) }
    // ADR-0032 piece B2b — scope for the mid-call camera-on
    // (`session.addLocalVideo()` is a suspend function launched from the
    // camera button's callback). Tied to this composition; torn down with
    // the screen.
    val cameraScope = rememberCoroutineScope()
    DisposableEffect(Unit) {
        onDispose {
            sounds.release()
            state.reset()
        }
    }

    // G3-2-session-bind — drive the UI from the real session
    // when one is injected; otherwise the prejoin →
    // seedMockState() path runs as before.
    LaunchedEffect(session) {
        if (session != null) {
            state.driveFrom(session, remoteName = remoteName.orEmpty())
        }
    }

    val phase by state.phase.collectAsState()
    val participants by state.participants.collectAsState()

    // Phase → sound mapping. Mirrors the same trigger map on
    // web (`Call.tsx`) and iOS (`CallScreen.swift`) so the
    // audible UX is identical across platforms.
    LaunchedEffect(phase) {
        when (phase) {
            is CallUiState.CallPhase.Prejoin -> sounds.stopAll()
            is CallUiState.CallPhase.Connecting ->
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
                    state.addSystemEvent("${p.displayName} joined the call")
                }
            }
            val currentIds = remoteIds.toSet()
            for (prevId in lastRemoteIds) {
                if (prevId !in currentIds) {
                    sounds.playOnce(CallSoundController.SoundName.LEAVE)
                    state.addSystemEvent("A participant left the call")
                    // Display name has been dropped from state
                    // by the time we observe the deletion;
                    // future refinement preserves it. Out of
                    // scope for S-3-A-chat.
                }
            }
        }
        lastRemoteIds = remoteIds
    }

    LakkoTheme(dark = true) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(LakkoDark.background)
            .windowInsetsPadding(WindowInsets.safeDrawing),
    ) {
    when (phase) {
        is CallUiState.CallPhase.Prejoin ->
            PrejoinScreen(
                state = state,
                onPrejoinJoin = onPrejoinJoin,
                onPrejoinCancel = onPrejoinCancel,
            )
        else -> InCallView(
            state = state,
            onExit = onExit,
            // G3-2-session-bind / G3-2-call-controls — route the
            // end-call + mic/camera toggles to the real session
            // when one drives the screen; mock-state fallback
            // otherwise.
            onEndCall = {
                when {
                    session != null -> session.hangUp()
                    onEndCall != null -> onEndCall()
                    else -> state.endCall()
                }
            },
            onSetMic = { on -> if (session != null) session.setMicrophoneEnabled(on) else state.setMic(on) },
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
                        session.setCameraEnabled(false)
                    }
                } else {
                    state.setCamera(on)
                }
            },
        )
    }
    }
    }
}

// ============================================================
// PrejoinScreen
// ============================================================

@Composable
private fun PrejoinScreen(
    state: CallUiState,
    onPrejoinJoin: ((micOn: Boolean, camOn: Boolean) -> Unit)? = null,
    onPrejoinCancel: (() -> Unit)? = null,
) {
    var camOn by remember { mutableStateOf(true) }
    var micOn by remember { mutableStateOf(true) }
    // G3-2-video-ui-screen — observe the live local track for
    // the rare case where the host has armed the camera before
    // the user hits Join (e.g. the accept-with-video flow runs
    // through the prejoin screen).
    val localVideoTrack by state.localVideoTrack.collectAsState()
    val settings by state.settings.collectAsState()

    // docs/ux/client-redesign.md 5.7 — the prejoin is one card on the dark
    // ground: the preview, the two toggles, Join. The camera and microphone
    // choices are carried into `dial`, so they drive the real tracks.
    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(LakkoDark.background)
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(20.dp, Alignment.CenterVertically),
    ) {
        Text(
            "Join call",
            color = LakkoDark.text,
            fontSize = 22.sp,
            fontWeight = FontWeight.SemiBold,
        )
        Column(
            modifier = Modifier
                .widthIn(max = 480.dp)
                .fillMaxWidth()
                .background(LakkoDark.surface, RoundedCornerShape(12.dp))
                .border(1.dp, LakkoDark.border, RoundedCornerShape(12.dp))
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .aspectRatio(16f / 9f)
                    .background(LakkoDark.sunken, RoundedCornerShape(12.dp)),
                contentAlignment = Alignment.Center,
            ) {
                if (camOn) {
                    val eglContext = remember { WebRtcPeerConnection.sharedEglContext() }
                    val track = localVideoTrack
                    if (track != null && eglContext != null) {
                        LocalVideoPreviewView(
                            track = track,
                            eglContext = eglContext,
                            modifier = Modifier.fillMaxSize(),
                            mirrored = settings.mirrorLocal,
                            scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
                        )
                    } else {
                        Text(
                            "Camera preview",
                            color = LakkoDark.textMuted,
                            fontSize = 13.sp,
                        )
                    }
                } else {
                    Icon(
                        Icons.Filled.VideocamOff,
                        contentDescription = null,
                        tint = LakkoDark.textMuted,
                        modifier = Modifier.size(48.dp),
                    )
                }
            }
            PrejoinToggleRow(
                icon = if (camOn) Icons.Filled.Videocam else Icons.Filled.VideocamOff,
                label = "Camera",
                checked = camOn,
                onCheckedChange = { camOn = it },
            )
            PrejoinToggleRow(
                icon = if (micOn) Icons.Filled.Mic else Icons.Filled.MicOff,
                label = "Microphone",
                checked = micOn,
                onCheckedChange = { micOn = it },
            )
            Button(
                onClick = {
                    // G3-2-prejoin-join — with a real session, Join dials
                    // it carrying the chosen mic / camera. Without one
                    // (mock design path) seed the mock state as before.
                    if (onPrejoinJoin != null) {
                        onPrejoinJoin(micOn, camOn)
                    } else {
                        state.seedMockState()
                        state.setMic(micOn)
                        state.setCamera(camOn)
                        state.join()
                    }
                },
                colors = ButtonDefaults.buttonColors(
                    containerColor = LakkoDark.accent,
                    contentColor = LakkoDark.accentFg,
                ),
                shape = RoundedCornerShape(8.dp),
                modifier = Modifier
                    .fillMaxWidth()
                    .height(48.dp),
            ) {
                Text("Join", fontWeight = FontWeight.SemiBold, fontSize = 16.sp)
            }
        }

        // G3-2-prejoin-join — Cancel abandons a prepped-but-undialed
        // call (real session only); absent on the mock design path.
        if (onPrejoinCancel != null) {
            TextButton(onClick = onPrejoinCancel) {
                Text("Cancel", color = LakkoDark.textMuted, fontSize = 14.sp)
            }
        }
    }
}

/** One labelled toggle of the prejoin card: an icon, the label, a switch. */
@Composable
private fun PrejoinToggleRow(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(
            icon,
            contentDescription = null,
            tint = if (checked) LakkoDark.text else LakkoDark.textMuted,
            modifier = Modifier.size(22.dp),
        )
        Text(
            label,
            color = LakkoDark.text,
            fontSize = 15.sp,
            modifier = Modifier.weight(1f),
        )
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange,
            colors = SwitchDefaults.colors(
                checkedThumbColor = LakkoDark.accentFg,
                checkedTrackColor = LakkoDark.accent,
                uncheckedThumbColor = LakkoDark.textMuted,
                uncheckedTrackColor = LakkoDark.sunken,
                uncheckedBorderColor = LakkoDark.border,
            ),
        )
    }
}

// ============================================================
// InCallView
// ============================================================

@Composable
private fun InCallView(
    state: CallUiState,
    onExit: () -> Unit,
    onEndCall: () -> Unit = { state.endCall() },
    onSetMic: (Boolean) -> Unit = { state.setMic(it) },
    onSetCamera: (Boolean) -> Unit = { state.setCamera(it) },
) {
    val participants by state.participants.collectAsState()
    val phase by state.phase.collectAsState()
    val drawerOpen by state.participantListOpen.collectAsState()
    val e2ee by state.e2ee.collectAsState()

    LaunchedEffect(phase) {
        if (phase is CallUiState.CallPhase.Ended) {
            delay(3_000)
            onExit()
        }
    }

    Box(modifier = Modifier.fillMaxSize().background(LakkoDark.background)) {
        Column(modifier = Modifier.fillMaxSize()) {
            // Header bar: E2EE pill + quality indicator.
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(LakkoDark.surface)
                    .border(1.dp, LakkoDark.border)
                    .padding(horizontal = 14.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                E2eePill(e2ee)
                Spacer(Modifier.weight(1f))
                QualityIndicator(state)
            }

            // Main scrollable area.
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .clickable(
                        indication = null,
                        interactionSource = remember { androidx.compose.foundation.interaction.MutableInteractionSource() },
                    ) {
                        // Tap-outside-to-close for the quality popover.
                        if (state.qualityPopoverOpen.value) {
                            state.setQualityPopoverOpen(false)
                        }
                    },
            ) {
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
                        .padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    if (active != null) {
                        ParticipantTile(
                            participant = active,
                            large = true,
                            state = state,
                        )
                    }
                    gallery.chunked(2).forEach { row ->
                        Row(
                            modifier = Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                        ) {
                            row.forEach { p ->
                                key(p.id) {
                                    Box(modifier = Modifier.weight(1f)) {
                                        ParticipantTile(
                                            participant = p,
                                            large = false,
                                            state = state,
                                        )
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
                    Box(
                        modifier = Modifier.fillMaxSize(),
                        contentAlignment = Alignment.Center,
                    ) {
                        Box(
                            modifier = Modifier
                                .background(
                                    LakkoDark.surface.copy(alpha = 0.9f),
                                    RoundedCornerShape(12.dp),
                                )
                                .border(1.dp, LakkoDark.border, RoundedCornerShape(12.dp))
                                .padding(horizontal = 24.dp, vertical = 16.dp),
                        ) {
                            Text(
                                "Waiting for others to join...",
                                color = LakkoDark.text,
                                fontSize = 14.sp,
                            )
                        }
                    }
                }
            }

            ControlsBar(
                state,
                onEndCall = onEndCall,
                onSetMic = onSetMic,
                onSetCamera = onSetCamera,
            )
        }

        // Top-centre phase banner.
        PhaseBanner(phase, modifier = Modifier
            .align(Alignment.TopCenter)
            .padding(top = 60.dp))

        // Right-edge drawer.
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

        // Ended overlay (auto-navigates after 3s).
        if (phase is CallUiState.CallPhase.Ended) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(LakkoDark.background.copy(alpha = 0.85f)),
                contentAlignment = Alignment.Center,
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(
                        "Call ended",
                        color = LakkoDark.text,
                        fontSize = 32.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        "Returning to your chats...",
                        color = LakkoDark.textMuted,
                        fontSize = 14.sp,
                    )
                }
            }
        }

        // Gear-menu bottom sheet (S-3-A-settings).
        SettingsSheet(state)
        // Chat bottom sheet (S-3-A-chat).
        ChatSheet(state)
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

@Composable
private fun E2eePill(state: CallUiState.E2eeState) {
    // Green (verified-comparable) only when keying is real AND a safety
    // number is present to read out-of-band; otherwise yellow, so "E2EE
    // active but no comparable number" stays visually distinct from
    // "active + number present" (matches the web pill's gate).
    val verified = state.active && state.shortFingerprint != null
    val activeColour = if (verified) LakkoDark.success else LakkoDark.warning
    Row(
        modifier = Modifier
            .background(LakkoDark.surface, RoundedCornerShape(50))
            .border(1.dp, LakkoDark.border, RoundedCornerShape(50))
            .padding(horizontal = 8.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Icon(
            Icons.Filled.Lock,
            contentDescription = "End-to-end encryption indicator",
            tint = activeColour,
            modifier = Modifier.size(12.dp),
        )
        Text(
            text = if (state.active && state.shortFingerprint != null) state.shortFingerprint else "…",
            color = activeColour,
            fontSize = 11.sp,
            fontWeight = FontWeight.SemiBold,
            fontFamily = FontFamily.Monospace,
        )
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
                    mirrored = participant.isLocal && settings.mirrorLocal,
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

@OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
private fun ControlsBar(
    state: CallUiState,
    onEndCall: () -> Unit = { state.endCall() },
    onSetMic: (Boolean) -> Unit = { state.setMic(it) },
    onSetCamera: (Boolean) -> Unit = { state.setCamera(it) },
) {
    val participants by state.participants.collectAsState()
    val hasMultipleCams by state.hasMultipleCameras.collectAsState()
    val local = participants.firstOrNull { it.isLocal }
    val drawerOpen by state.participantListOpen.collectAsState()
    val settingsOpen by state.settingsOpen.collectAsState()
    // ADR-0032 piece B2a — while the mid-call camera-on acquire + renegotiation
    // are in flight, the camera button spins + disables (the session already
    // in-flight-guards; this is the visual half).
    val videoPublishState by state.videoPublishState.collectAsState()
    val cameraStarting = videoPublishState == CallUiState.VideoPublishState.Starting

    val chatOpen by state.chatOpen.collectAsState()
    val chatUnread by state.chatUnreadCount.collectAsState()
    var moreOpen by remember { mutableStateOf(false) }

    // docs/ux/client-redesign.md 5.7 — one row of 56 dp round buttons on the
    // sunken dark surface: microphone, camera, More, End call. Everything else
    // lives in the More menu, the same shape as the iOS phone-width layout, so
    // End call is always on screen.
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(LakkoDark.sunken)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CircleButton(
            icon = if (local?.micOn == true) Icons.Filled.Mic else Icons.Filled.MicOff,
            active = local?.micOn == false,
            description = if (local?.micOn == true) "Mute microphone" else "Unmute microphone",
            onClick = { onSetMic(local?.micOn != true) },
        )
        CircleButton(
            icon = if (local?.camOn == true) Icons.Filled.Videocam else Icons.Filled.VideocamOff,
            active = local?.camOn == false,
            description = when {
                cameraStarting -> "Starting camera..."
                local?.camOn == true -> "Stop camera"
                else -> "Start camera"
            },
            onClick = { onSetCamera(local?.camOn != true) },
            busy = cameraStarting,
            enabled = !cameraStarting,
        )
        Box {
            CircleButton(
                icon = Icons.Filled.MoreHoriz,
                active = local?.handRaised == true || local?.screenSharing == true,
                description = if (chatUnread > 0 && !chatOpen) {
                    "More controls, $chatUnread unread chat messages"
                } else {
                    "More controls"
                },
                onClick = { moreOpen = true },
            )
            if (chatUnread > 0 && !chatOpen) {
                Box(
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .size(12.dp)
                        .background(LakkoDark.danger, CircleShape),
                )
            }
            DropdownMenu(expanded = moreOpen, onDismissRequest = { moreOpen = false }) {
                DropdownMenuItem(
                    text = { Text("Participants") },
                    leadingIcon = { Icon(Icons.Filled.Group, contentDescription = null) },
                    onClick = {
                        moreOpen = false
                        state.setParticipantListOpen(!drawerOpen)
                    },
                )
                DropdownMenuItem(
                    text = { Text(if (chatUnread > 0) "Chat ($chatUnread unread)" else "Chat") },
                    leadingIcon = { Icon(Icons.Filled.Chat, contentDescription = null) },
                    onClick = {
                        moreOpen = false
                        state.setChatOpen(!chatOpen)
                    },
                )
                DropdownMenuItem(
                    text = { Text(if (local?.handRaised == true) "Lower hand" else "Raise hand") },
                    leadingIcon = { Icon(Icons.Filled.PanTool, contentDescription = null) },
                    onClick = {
                        moreOpen = false
                        state.setHandRaised(local?.handRaised != true)
                    },
                )
                DropdownMenuItem(
                    text = {
                        Text(if (local?.screenSharing == true) "Stop sharing screen" else "Share screen")
                    },
                    leadingIcon = { Icon(Icons.Filled.ScreenShare, contentDescription = null) },
                    onClick = {
                        moreOpen = false
                        state.setScreenSharing(local?.screenSharing != true)
                    },
                )
                if (hasMultipleCams && local?.camOn == true) {
                    DropdownMenuItem(
                        text = { Text("Switch camera") },
                        leadingIcon = { Icon(Icons.Filled.FlipCameraAndroid, contentDescription = null) },
                        onClick = {
                            moreOpen = false
                            /* WebRTC pod owns facingMode toggle (3a-pod-followup) */
                        },
                    )
                }
                DropdownMenuItem(
                    text = { Text("Settings") },
                    leadingIcon = { Icon(Icons.Filled.Settings, contentDescription = null) },
                    onClick = {
                        moreOpen = false
                        state.setSettingsOpen(!settingsOpen)
                    },
                )
            }
        }
        CircleButton(
            icon = Icons.Filled.CallEnd,
            active = false,
            destructive = true,
            description = "End call",
            onClick = onEndCall,
        )
    }
}

@Composable
private fun CircleButton(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    active: Boolean,
    description: String,
    onClick: () -> Unit,
    destructive: Boolean = false,
    // ADR-0032 piece B2a — while an async action is in flight (mid-call
    // camera-on), render a spinner in place of the icon and disable the tap.
    busy: Boolean = false,
    enabled: Boolean = true,
) {
    val bg = when {
        destructive -> LakkoDark.danger
        active -> LakkoDark.accent
        else -> LakkoDark.surface
    }
    val fg = when {
        destructive -> LakkoDark.dangerFg
        active -> LakkoDark.accentFg
        else -> LakkoDark.text
    }
    IconButton(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier
            .size(56.dp)
            .background(bg, CircleShape),
    ) {
        if (busy) {
            CircularProgressIndicator(
                modifier = Modifier.size(20.dp),
                color = fg,
                strokeWidth = 2.dp,
            )
        } else {
            Icon(icon, contentDescription = description, tint = fg)
        }
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

@Composable
private fun PhaseBanner(phase: CallUiState.CallPhase, modifier: Modifier = Modifier) {
    val info: Pair<String, Color>? = when (phase) {
        is CallUiState.CallPhase.Prejoin -> null
        is CallUiState.CallPhase.Connected -> null
        is CallUiState.CallPhase.Connecting -> "Connecting..." to LakkoDark.accent
        is CallUiState.CallPhase.Reconnecting ->
            "Reconnecting..." to LakkoDark.warning
        is CallUiState.CallPhase.Ended -> "Call ended" to LakkoDark.danger
    }
    info?.let { (text, colour) ->
        Box(
            modifier = modifier
                .background(colour, RoundedCornerShape(50))
                .padding(horizontal = 14.dp, vertical = 6.dp),
        ) {
            Text(
                text,
                color = LakkoDark.accentFg,
                fontSize = 11.sp,
                fontWeight = FontWeight.SemiBold,
            )
        }
    }
}
