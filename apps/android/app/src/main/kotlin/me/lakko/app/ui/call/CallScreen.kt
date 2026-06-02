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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Brush
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
import androidx.compose.ui.platform.LocalContext
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

// ============================================================
// Theme tokens (dark) — matches web Call.tsx + iOS CallScreen.
// ============================================================

private object CallTheme {
    val background = Color(0xFF0F0F0F)
    val surface = Color(0xFF1A1A1A)
    val border = Color(0xFF2A2A2A)
    val textPrimary = Color(0xFFFAFAFA)
    val textSecondary = Color(0xFFA1A1AA)
    val accentBlue = Color(0xFF3B82F6)
    val endCallRed = Color(0xFFDC2626)
    val mutedRed = Color(0xFFEF4444)
    val qualityGreen = Color(0xFF10B981)
    val qualityYellow = Color(0xFFFBBF24)
}

private fun CallUiState.QualityTier.colour(): Color = when (this) {
    CallUiState.QualityTier.Excellent -> CallTheme.qualityGreen
    CallUiState.QualityTier.Good, CallUiState.QualityTier.Fair -> CallTheme.qualityYellow
    CallUiState.QualityTier.Poor, CallUiState.QualityTier.Bad -> CallTheme.mutedRed
}

// ============================================================
// CallScreen — top-level composable
// ============================================================

@Composable
public fun CallScreen(
    onExit: () -> Unit,
    state: CallUiState = remember { CallUiState() },
) {
    // One sound controller per CallScreen lifetime; released
    // in the same DisposableEffect that resets the UI state so
    // the SoundPool + cached MediaPlayer handles do not leak
    // their AudioTrack resources when the user navigates away.
    val context = LocalContext.current
    val sounds = remember(context) { CallSoundController(context) }
    DisposableEffect(Unit) {
        onDispose {
            sounds.release()
            state.reset()
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

    when (phase) {
        is CallUiState.CallPhase.Prejoin -> PrejoinScreen(state)
        else -> InCallView(state = state, onExit = onExit)
    }
}

// ============================================================
// PrejoinScreen
// ============================================================

@Composable
private fun PrejoinScreen(state: CallUiState) {
    var displayName by remember { mutableStateOf("") }
    var camOn by remember { mutableStateOf(true) }
    var micOn by remember { mutableStateOf(true) }
    // G3-2-video-ui-screen — observe the live local track for
    // the rare case where the host has armed the camera before
    // the user hits Join (e.g. the accept-with-video flow runs
    // through the prejoin screen).
    val localVideoTrack by state.localVideoTrack.collectAsState()
    val settings by state.settings.collectAsState()

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(CallTheme.background)
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(20.dp, Alignment.CenterVertically),
    ) {
        Text(
            "Join Call",
            color = CallTheme.textPrimary,
            fontSize = 24.sp,
            fontWeight = FontWeight.SemiBold,
        )
        Text(
            "Set up your camera and microphone",
            color = CallTheme.textSecondary,
            fontSize = 14.sp,
        )

        Box(
            modifier = Modifier
                .widthIn(max = 480.dp)
                .fillMaxWidth()
                .aspectRatio(16f / 9f)
                .background(CallTheme.surface, RoundedCornerShape(12.dp))
                .border(1.dp, CallTheme.border, RoundedCornerShape(12.dp)),
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
                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .background(
                                Brush.linearGradient(
                                    colors = listOf(
                                        Color(0xFF1F2937),
                                        Color(0xFF111827),
                                        Color(0xFF0B0F19),
                                    ),
                                ),
                                RoundedCornerShape(12.dp),
                            ),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            "(camera preview)",
                            color = CallTheme.textSecondary,
                            fontSize = 13.sp,
                        )
                    }
                }
            } else {
                Box(
                    modifier = Modifier.fillMaxSize(),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        Icons.Filled.VideocamOff,
                        contentDescription = null,
                        tint = CallTheme.textSecondary,
                        modifier = Modifier.size(48.dp),
                    )
                }
            }

            // Cam / mic toggle overlay in the top-right.
            Row(
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .padding(10.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                PreviewToggleButton(
                    icon = if (camOn) Icons.Filled.Videocam else Icons.Filled.VideocamOff,
                    active = camOn,
                    description = if (camOn) "Turn off camera" else "Turn on camera",
                    onClick = { camOn = !camOn },
                )
                PreviewToggleButton(
                    icon = if (micOn) Icons.Filled.Mic else Icons.Filled.MicOff,
                    active = micOn,
                    description = if (micOn) "Mute microphone" else "Unmute microphone",
                    onClick = { micOn = !micOn },
                )
            }

            // Display-name input at the bottom.
            Box(
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .fillMaxWidth()
                    .background(Color.Black.copy(alpha = 0.45f))
                    .padding(12.dp),
            ) {
                BasicTextField(
                    value = displayName,
                    onValueChange = { displayName = it },
                    singleLine = true,
                    textStyle = TextStyle(
                        color = CallTheme.textPrimary,
                        fontSize = 15.sp,
                    ),
                    keyboardOptions = KeyboardOptions(
                        capitalization = KeyboardCapitalization.Words,
                    ),
                    decorationBox = { inner ->
                        Box(
                            modifier = Modifier
                                .fillMaxWidth()
                                .background(
                                    Color.White.copy(alpha = 0.08f),
                                    RoundedCornerShape(8.dp),
                                )
                                .border(1.dp, CallTheme.border, RoundedCornerShape(8.dp))
                                .padding(horizontal = 12.dp, vertical = 8.dp),
                        ) {
                            if (displayName.isEmpty()) {
                                Text(
                                    "Your name",
                                    color = CallTheme.textSecondary,
                                    fontSize = 15.sp,
                                )
                            }
                            inner()
                        }
                    },
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        }

        IconButton(
            onClick = {
                state.seedMockState()
                state.setMic(micOn)
                state.setCamera(camOn)
                // displayName unused — production maps via contact name lookup.
                state.join()
            },
            modifier = Modifier
                .background(CallTheme.accentBlue, RoundedCornerShape(10.dp))
                .padding(horizontal = 32.dp, vertical = 6.dp)
                .shadow(4.dp, RoundedCornerShape(10.dp)),
        ) {
            Text(
                "Join",
                color = CallTheme.textPrimary,
                fontWeight = FontWeight.SemiBold,
                fontSize = 16.sp,
            )
        }
    }
}

@Composable
private fun PreviewToggleButton(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    active: Boolean,
    description: String,
    onClick: () -> Unit,
) {
    IconButton(
        onClick = onClick,
        modifier = Modifier
            .size(36.dp)
            .background(
                if (active) Color.Black.copy(alpha = 0.55f) else CallTheme.mutedRed,
                CircleShape,
            ),
    ) {
        Icon(icon, contentDescription = description, tint = CallTheme.textPrimary)
    }
}

// ============================================================
// InCallView
// ============================================================

@Composable
private fun InCallView(state: CallUiState, onExit: () -> Unit) {
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

    Box(modifier = Modifier.fillMaxSize().background(CallTheme.background)) {
        Column(modifier = Modifier.fillMaxSize()) {
            // Header bar: E2EE pill + quality indicator.
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(CallTheme.surface)
                    .border(1.dp, CallTheme.border)
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
                @Suppress("UNUSED_VARIABLE") val _participants_dep = participants
                LazyColumn(
                    modifier = Modifier.fillMaxSize().padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    state.activeSpeaker()?.let { active ->
                        item("active-${active.id}") {
                            ParticipantTile(
                                participant = active,
                                large = true,
                                state = state,
                            )
                        }
                    }
                    item("gallery") {
                        LazyVerticalGrid(
                            columns = GridCells.Adaptive(minSize = 180.dp),
                            verticalArrangement = Arrangement.spacedBy(10.dp),
                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                            modifier = Modifier.height(((state.galleryParticipants().size + 1) / 2 * 160).dp.coerceAtLeast(0.dp)),
                            contentPadding = PaddingValues(0.dp),
                        ) {
                            gridItems(state.galleryParticipants(), key = { it.id }) { p ->
                                ParticipantTile(
                                    participant = p,
                                    large = false,
                                    state = state,
                                )
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
                                    CallTheme.surface.copy(alpha = 0.9f),
                                    RoundedCornerShape(12.dp),
                                )
                                .border(1.dp, CallTheme.border, RoundedCornerShape(12.dp))
                                .padding(horizontal = 24.dp, vertical = 16.dp),
                        ) {
                            Text(
                                "Waiting for participant…",
                                color = CallTheme.textPrimary,
                                fontSize = 14.sp,
                            )
                        }
                    }
                }
            }

            ControlsBar(state)
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
                    .background(CallTheme.background.copy(alpha = 0.85f)),
                contentAlignment = Alignment.Center,
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(
                        "Call ended",
                        color = CallTheme.textPrimary,
                        fontSize = 32.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        "Returning to your chats…",
                        color = CallTheme.textSecondary,
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
        containerColor = CallTheme.surface,
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
                color = CallTheme.textPrimary,
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

            Divider(color = CallTheme.border, modifier = Modifier.padding(vertical = 4.dp))

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
        color = CallTheme.textSecondary,
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
        Text(label, color = CallTheme.textPrimary, fontSize = 13.sp)
        Box {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(CallTheme.background, RoundedCornerShape(8.dp))
                    .border(1.dp, CallTheme.border, RoundedCornerShape(8.dp))
                    .clickable { expanded = true }
                    .padding(horizontal = 10.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    selectedLabel,
                    color = CallTheme.textPrimary,
                    fontSize = 14.sp,
                    modifier = Modifier.weight(1f),
                )
                Icon(
                    Icons.Filled.ArrowDropDown,
                    contentDescription = null,
                    tint = CallTheme.textSecondary,
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
                color = CallTheme.textSecondary,
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
            Text(label, color = CallTheme.textPrimary, fontSize = 14.sp)
            if (description != null) {
                Text(description, color = CallTheme.textSecondary, fontSize = 11.sp)
            }
        }
        Switch(checked = checked, onCheckedChange = onChange)
    }
}

// ============================================================
// ChatToggleButton + ChatSheet (S-3-A-chat)
// ============================================================

/** CircleButton with an overlaid unread-count badge. The
 *  badge surfaces only when there is something unread AND the
 *  panel is closed. */
@Composable
private fun ChatToggleButton(state: CallUiState) {
    val chatOpen by state.chatOpen.collectAsState()
    val unread by state.chatUnreadCount.collectAsState()

    Box {
        CircleButton(
            icon = Icons.Filled.Chat,
            active = chatOpen,
            description = when {
                chatOpen -> "Close chat"
                unread > 0 -> "Chat ($unread unread)"
                else -> "Chat"
            },
            onClick = { state.setChatOpen(!chatOpen) },
        )
        if (unread > 0 && !chatOpen) {
            Box(
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .offset(x = 2.dp, y = (-2).dp)
                    .background(CallTheme.endCallRed, CircleShape)
                    .padding(horizontal = 5.dp, vertical = 1.dp)
                    .widthIn(min = 18.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    if (unread > 99) "99+" else unread.toString(),
                    color = Color.White,
                    fontSize = 10.sp,
                    fontWeight = FontWeight.Bold,
                )
            }
        }
    }
}

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
        containerColor = CallTheme.surface,
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
                color = CallTheme.textPrimary,
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
                        "No messages yet. Anything you send is end-to-end encrypted " +
                            "inside this call's MLS group — the server cannot read it.",
                        color = CallTheme.textSecondary,
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
            color = CallTheme.textSecondary,
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
                    color = CallTheme.accentBlue,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    formatChatTimestamp(msg.tsMs),
                    color = CallTheme.textSecondary,
                    fontSize = 12.sp,
                )
            }
            Text(
                msg.text,
                color = CallTheme.textPrimary,
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
    val activeColour = if (state.active) CallTheme.qualityGreen else CallTheme.qualityYellow
    Row(
        modifier = Modifier
            .background(CallTheme.surface, RoundedCornerShape(50))
            .border(1.dp, CallTheme.border, RoundedCornerShape(50))
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
                    .background(CallTheme.surface, RoundedCornerShape(8.dp))
                    .border(1.dp, CallTheme.border, RoundedCornerShape(8.dp))
                    .padding(10.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(
                        tier.name,
                        color = CallTheme.textPrimary,
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
                            CallUiState.ConnectionDetail.ConnectionType.Direct -> CallTheme.qualityGreen
                            CallUiState.ConnectionDetail.ConnectionType.Relay -> CallTheme.qualityYellow
                            CallUiState.ConnectionDetail.ConnectionType.Unknown -> CallTheme.textSecondary
                        },
                        fontSize = 12.sp,
                    )
                }
                Divider(color = CallTheme.border)
                Text(
                    "RTT: ${detail.rttMs?.toString()?.plus(" ms") ?: "—"}",
                    color = CallTheme.textPrimary,
                    fontSize = 11.sp,
                )
                Text(
                    "↓ ${formatBps(detail.downBps)}   ↑ ${formatBps(detail.upBps)}",
                    color = CallTheme.textPrimary,
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
    val participants by state.participants.collectAsState()
    val settings by state.settings.collectAsState()
    val soleRemoteId = remember(participants) { state.soleRemoteParticipantId }

    Box(
        modifier = Modifier
            .fillMaxWidth()
            .aspectRatio(if (large) 16f / 9f else 4f / 3f)
            .background(CallTheme.surface, RoundedCornerShape(12.dp))
            .border(
                width = if (participant.speaking) 2.dp else 1.dp,
                color = if (participant.speaking) CallTheme.accentBlue else CallTheme.border,
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
            val isSoleRemote = !participant.isLocal && participant.id == soleRemoteId
            val track = when {
                participant.isLocal -> localVideoTrack
                isSoleRemote -> remoteVideoTrack
                else -> null
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
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .background(
                            Brush.linearGradient(
                                colors = listOf(
                                    Color(0xFF1F2937),
                                    Color(0xFF111827),
                                    Color(0xFF0B0F19),
                                ),
                            ),
                            RoundedCornerShape(12.dp),
                        ),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        "(video)",
                        color = CallTheme.textSecondary.copy(alpha = 0.6f),
                        fontSize = 12.sp,
                    )
                }
            }
        } else {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(Color(0xFF27272A), RoundedCornerShape(12.dp)),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = participant.displayName.firstOrNull()?.uppercase() ?: "?",
                    color = CallTheme.textPrimary,
                    fontSize = if (large) 80.sp else 40.sp,
                    fontWeight = FontWeight.SemiBold,
                )
            }
        }

        // Centre mute overlay for muted remotes.
        if (!participant.micOn && !participant.isLocal) {
            Box(
                modifier = Modifier
                    .align(Alignment.Center)
                    .size(if (large) 56.dp else 36.dp)
                    .background(Color.Black.copy(alpha = 0.55f), CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Filled.MicOff,
                    contentDescription = "Muted",
                    tint = CallTheme.mutedRed,
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
                    .background(Color.Black.copy(alpha = 0.6f), RoundedCornerShape(6.dp))
                    .padding(horizontal = 6.dp, vertical = 2.dp),
            ) {
                Text("Sharing", color = CallTheme.textPrimary, fontSize = 10.sp)
            }
        }

        // Top-right hand badge.
        if (participant.handRaised) {
            Box(
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .padding(8.dp)
                    .background(Color.Black.copy(alpha = 0.6f), RoundedCornerShape(6.dp))
                    .padding(4.dp),
            ) {
                Text("✋", fontSize = 14.sp)
            }
        }

        // Bottom strip: name + mute + quality + pin.
        Row(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                .background(Color.Black.copy(alpha = 0.45f))
                .padding(horizontal = 10.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (!participant.micOn) {
                Icon(
                    Icons.Filled.MicOff,
                    contentDescription = "Muted",
                    tint = CallTheme.mutedRed,
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
                color = CallTheme.textPrimary,
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
                    tint = CallTheme.accentBlue,
                    modifier = Modifier.size(12.dp),
                )
            }
        }
    }
}

@Composable
private fun ControlsBar(state: CallUiState) {
    val participants by state.participants.collectAsState()
    val hasMultipleCams by state.hasMultipleCameras.collectAsState()
    val local = participants.firstOrNull { it.isLocal }
    val drawerOpen by state.participantListOpen.collectAsState()
    val settingsOpen by state.settingsOpen.collectAsState()

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(CallTheme.surface)
            .border(1.dp, CallTheme.border)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CircleButton(
            icon = if (local?.micOn == true) Icons.Filled.Mic else Icons.Filled.MicOff,
            active = local?.micOn == false,
            description = if (local?.micOn == true) "Mute microphone" else "Unmute microphone",
            onClick = { state.setMic(local?.micOn != true) },
        )
        CircleButton(
            icon = if (local?.camOn == true) Icons.Filled.Videocam else Icons.Filled.VideocamOff,
            active = local?.camOn == false,
            description = if (local?.camOn == true) "Stop camera" else "Start camera",
            onClick = { state.setCamera(local?.camOn != true) },
        )
        if (hasMultipleCams && local?.camOn == true) {
            CircleButton(
                icon = Icons.Filled.FlipCameraAndroid,
                active = false,
                description = "Switch camera",
                onClick = { /* WebRTC pod owns facingMode toggle (3a-pod-followup) */ },
            )
        }
        CircleButton(
            icon = Icons.Filled.Group,
            active = drawerOpen,
            description = "Participants",
            onClick = { state.setParticipantListOpen(!drawerOpen) },
        )
        CircleButton(
            icon = Icons.Filled.PanTool,
            active = local?.handRaised == true,
            description = if (local?.handRaised == true) "Lower hand" else "Raise hand",
            onClick = { state.setHandRaised(local?.handRaised != true) },
        )
        CircleButton(
            icon = Icons.Filled.ScreenShare,
            active = local?.screenSharing == true,
            description = if (local?.screenSharing == true) "Stop sharing screen" else "Share screen",
            onClick = { state.setScreenSharing(local?.screenSharing != true) },
        )
        ChatToggleButton(state)
        CircleButton(
            icon = Icons.Filled.Settings,
            active = settingsOpen,
            description = "Settings",
            onClick = { state.setSettingsOpen(!settingsOpen) },
        )
        CircleButton(
            icon = Icons.Filled.CallEnd,
            active = false,
            destructive = true,
            description = "End call",
            onClick = { state.endCall() },
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
) {
    val bg = when {
        destructive -> CallTheme.endCallRed
        active -> CallTheme.accentBlue
        else -> CallTheme.surface
    }
    IconButton(
        onClick = onClick,
        modifier = Modifier
            .size(52.dp)
            .background(bg, CircleShape)
            .shadow(4.dp, CircleShape),
    ) {
        Icon(icon, contentDescription = description, tint = CallTheme.textPrimary)
    }
}

@Composable
private fun ParticipantListDrawer(state: CallUiState) {
    val participants by state.participants.collectAsState()

    Column(
        modifier = Modifier
            .fillMaxHeight()
            .width(320.dp)
            .background(CallTheme.surface)
            .border(1.dp, CallTheme.border),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                "Participants (${participants.size})",
                color = CallTheme.textPrimary,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            IconButton(onClick = { state.setParticipantListOpen(false) }) {
                Icon(
                    Icons.Filled.Close,
                    contentDescription = "Close participant list",
                    tint = CallTheme.textSecondary,
                )
            }
        }
        Divider(color = CallTheme.border)
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
                        color = CallTheme.textPrimary,
                        modifier = Modifier.weight(1f),
                    )
                    if (p.handRaised) Text("✋")
                    if (!p.micOn) {
                        Icon(
                            Icons.Filled.MicOff,
                            contentDescription = "Muted",
                            tint = CallTheme.mutedRed,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                    if (p.screenSharing) {
                        Icon(
                            Icons.Filled.ScreenShare,
                            contentDescription = "Sharing screen",
                            tint = CallTheme.textPrimary,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                }
                Divider(color = CallTheme.border)
            }
        }
    }
}

@Composable
private fun PhaseBanner(phase: CallUiState.CallPhase, modifier: Modifier = Modifier) {
    val info: Pair<String, Color>? = when (phase) {
        is CallUiState.CallPhase.Prejoin -> null
        is CallUiState.CallPhase.Connected -> null
        is CallUiState.CallPhase.Connecting -> "Connecting…" to CallTheme.accentBlue
        is CallUiState.CallPhase.Reconnecting ->
            "Reconnecting — ${phase.reason}" to CallTheme.qualityYellow
        is CallUiState.CallPhase.Ended -> "Call ended" to CallTheme.endCallRed
    }
    info?.let { (text, colour) ->
        Box(
            modifier = modifier
                .background(colour, RoundedCornerShape(50))
                .padding(horizontal = 14.dp, vertical = 6.dp),
        ) {
            Text(
                text,
                color = Color.Black.copy(alpha = 0.9f),
                fontSize = 11.sp,
                fontWeight = FontWeight.SemiBold,
            )
        }
    }
}
