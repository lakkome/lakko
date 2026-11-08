// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentSize
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Divider
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.material.icons.Icons
import androidx.compose.runtime.key
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.FlipCameraAndroid
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateMapOf
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
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
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
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.calls.CallCue
import me.lakko.app.calls.CallCueTracker
import me.lakko.app.calls.GROUP_CONTROL_SIDE_MARGIN_DP
import me.lakko.app.calls.GroupCallRules
import me.lakko.app.calls.InCallInvite
import me.lakko.app.calls.SharedGroupCallRules
import me.lakko.app.calls.callStatusTextFor
import me.lakko.app.calls.connectionIsWorthShowing
import me.lakko.app.calls.CallAudioInput
import me.lakko.app.calls.CallAudioOutput
import me.lakko.app.calls.CallDeviceSelection
import me.lakko.app.calls.SettingsSectionKind
import me.lakko.app.calls.SettingsSelection
import me.lakko.app.calls.callPlaysOnLoudspeaker
import me.lakko.app.calls.callSettingsSheetDevices
import me.lakko.app.calls.callSettingsSections
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import me.lakko.app.calls.ParticipantsControl
import me.lakko.app.calls.ParticipantsMenuItem
import me.lakko.app.calls.participantsControl
import me.lakko.app.calls.participantsMenuItems
import me.lakko.app.calls.endOpensMenu
import me.lakko.app.calls.inviteStatusHoldMs
import me.lakko.app.calls.inviteStatusLine
import me.lakko.app.calls.ringbackShouldPlay
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.only
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.unit.Dp
import me.lakko.app.calls.CallAudioRouter
import me.lakko.app.calls.CallControlKind
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.CallStage
import me.lakko.app.calls.CameraVideoCapture
import me.lakko.app.calls.callControlRow
import me.lakko.app.calls.CALL_CHAT_COLUMN_WIDTH_DP
import me.lakko.app.calls.CallChatButton
import me.lakko.app.calls.CallChatPreview
import me.lakko.app.calls.callBarMicControl
import me.lakko.app.calls.callChatIsColumn
import me.lakko.app.calls.callChatPreviewShown
import me.lakko.app.calls.callScreenTitle
import me.lakko.app.calls.callStage
import me.lakko.app.calls.controlsMayAutoHide
import me.lakko.app.calls.showsSelfViewPip
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.theme.avatarTintFor
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.ui.theme.LakkoTheme
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import me.lakko.app.calls.CALL_ENCRYPTION_CHIP_LABEL
import me.lakko.app.calls.CALL_ENCRYPTION_SHEET_TITLE
import me.lakko.app.calls.CallSoundController
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.LocalVideoPreviewView
import me.lakko.app.calls.callEncryptionHint
import me.lakko.app.calls.callEncryptionSentence
import me.lakko.app.calls.callClockOrigin
import me.lakko.app.calls.callElapsedSecs
import me.lakko.app.calls.callStatusSpokenLabel
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
 * controls, same security UX (the lock chip and its sheet, the
 * quality dot popover).
 *
 * State lives in [CallUiState] — the Kotlin port of the Solid
 * signals state machine. Compose subscribes via
 * `collectAsState` (kept inline; Compose recomposes when the
 * StateFlow emits).
 *
 * A group call's tiles are `GroupCallArea.kt`; its control row is
 * `CallControlBar.kt`.
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
    /** ADR-0084 — the other person's signature key in a 1x1 call, so the
     *  portrait shows their profile photo. */
    personKey: ByteArray? = null,
    /** The End button for a call the app-level host drives through [state]
     *  alone (a group or call-link call): the host leaves the call and the
     *  session's own closed transition shows the ended state. `null` keeps the
     *  session hang-up (1x1) or, with no session, the mock end. */
    onEndCall: (() -> Unit)? = null,
    /**
     * `docs/ux/calls.md` section 4.1.2 — end the call for every person in it.
     * Given only to the person who started a group or link call; while anyone
     * else is in the call, `End` then opens a choice between leaving
     * ([onEndCall]) and this. `null` for everyone else and for a one-to-one
     * call, where `End` ends at once.
     */
    onEndForEveryone: (() -> Unit)? = null,
    /**
     * Section 4.1.2 — the line under `Call ended` for a person whose call was
     * ended for everyone by its starter, or `null`.
     */
    endedForEveryoneLine: String? = null,
    /**
     * `docs/ux/calls.md` section 4.1 — flip to the other camera. It lives on
     * the self-view rectangle, not in the control row, so this is wired only
     * where a session can actually swap the capture; `null` leaves the button
     * off the self-view entirely.
     */
    onSwitchCamera: (() -> Unit)? = null,
    /**
     * `docs/ux/calls.md` section 4.5 — put the call away into the bar across the
     * top of the app, without ending it. Draws the chevron at the top left.
     *
     * `null` leaves the chevron off, which is right for a screen with no bar to
     * collapse into (the mock design route) — never for a live call.
     */
    onCollapse: (() -> Unit)? = null,
    /**
     * `docs/ux/calls.md` section 4.5 — the moment the CALL's media first flowed,
     * held by the host that owns the call.
     *
     * Load-bearing, and the reason is that this screen is destroyed and rebuilt
     * every time the call is put away and brought back. It used to start its
     * clock from the moment it observed the connect, so a twelve-minute call
     * came back reading `0:00` — see [callClockOrigin].
     *
     * Required and deliberately without a default. `null` is a meaningful value
     * — the call has not connected — but OMITTING it never is, and omitting it
     * would silently restore the defect by falling back to the state's own
     * latch, which for the 1x1 screen is the mount moment in disguise. No unit
     * test on this client can reach a composable's argument list; the compiler
     * can, so this is the one guard available.
     */
    connectedSinceMs: Long?,
    /**
     * This is a group or call-link call, whatever the roster happens to hold
     * right now. It keeps the tiles and the roster on screen while the second
     * participant is still arriving, and it is what tells the call it rings
     * nobody; see [callStage] and [ringbackShouldPlay].
     */
    groupCall: Boolean = false,
    /**
     * `docs/ux/calls.md` section 4 — what the call is called. For a group call
     * this is the group's name (ADR-0051); `null` means the group has none. It
     * is never derived from a participant: see [callScreenTitle].
     */
    groupName: String? = null,
    /**
     * `docs/ux/call-links.md` section 3 — what `Invite` is doing, or `null` for
     * a call that has no link to offer.
     *
     * A 1x1 call and a call in a chat group both pass `null`: their membership
     * is the chat's, and a `/c/` capability minted over one would send whoever
     * followed it into a different MLS group with a different key, hearing
     * nothing. Only the link call started by `New call`, and only on the device
     * that hosts it, has a link that leads back to the call on screen.
     */
    invite: InCallInvite? = null,
    /** `Copy link`: the link goes to the clipboard, minted on first use. */
    onCopyInviteLink: () -> Unit = {},
    /** `Invite contacts`: open the list of conversations to send the
     *  invitation into. `null` leaves the item out of the menu, for someone
     *  with no conversations. */
    onInviteContact: (() -> Unit)? = null,
    /**
     * The call's own record of which cues it has played, when the call does
     * not live in [state]: the one-to-one screen builds its state anew every
     * time the call is brought back from the bar, and would play the connected
     * sound again each time. `null` uses the state's.
     */
    cues: CallCueTracker? = null,
    /**
     * What the person chose in the call's Settings sheet, held by whatever
     * owns a call whose state this screen builds itself (the one-to-one
     * overlay), for the same reason as [cues]: a choice holds for the call,
     * not for one visit to its screen. `null` leaves it to the state.
     */
    deviceSelection: CallDeviceSelection? = null,
    /** The shared call rules; replaced only by a test. */
    rules: GroupCallRules = SharedGroupCallRules,
) {
    // Captured once: whether this composition created the state, and may
    // therefore reset it. Reading the parameter later would be wrong — the
    // question is who made it, which cannot change.
    val ownsState = remember { state == null }
    val callState = state ?: remember {
        // A state this screen makes starts from the session as it stands, so a
        // screen brought back to a running call does not open on `Calling...`.
        CallUiState(devices = deviceSelection).also { fresh ->
            session?.let { fresh.syncFrom(it, remoteName.orEmpty()) }
        }
    }
    // One sound controller per CallScreen lifetime; released
    // in the same DisposableEffect that resets the UI state so
    // the SoundPool + cached MediaPlayer handles do not leak
    // their AudioTrack resources when the user navigates away.
    val context = LocalContext.current
    val sounds = remember(context) { CallSoundController(context) }
    // docs/ux/calls.md section 4.1 — where the call's sound comes out and
    // which microphone it takes. There is no Speaker button: the route is a
    // rule, and the Settings sheet is where a person chooses otherwise.
    val audio = remember(context) { CallAudioRouter(context) }
    // ADR-0032 piece B2b — scope for the mid-call camera-on
    // (`session.addLocalVideo()` is a suspend function launched from the
    // camera button's callback). Tied to this composition; torn down with
    // the screen.
    val cameraScope = rememberCoroutineScope()
    DisposableEffect(Unit) {
        // A group call, and any call with video, plays on the loudspeaker and
        // lets a connected headset take over; a one-to-one voice call plays on
        // the earpiece. What the person chose earlier in this call is put back.
        audio.start(
            loudspeaker = callPlaysOnLoudspeaker(
                groupCall = groupCall,
                videoPresent = callState.participants.value.any { it.camOn },
            ),
            selectedOutputId = callState.devices.audioOutput.value,
            selectedInputId = callState.devices.audioInput.value,
        )
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

    // A camera coming on moves a one-to-one call to the loudspeaker, and the
    // last one going off moves it back to the earpiece.
    val videoPresent = participants.any { it.camOn }
    LaunchedEffect(groupCall, videoPresent) {
        audio.setLoudspeakerDefault(callPlaysOnLoudspeaker(groupCall, videoPresent))
    }

    // The outgoing tone (`docs/ux/calls.md` section 3.4). The shared rule
    // decides: only a one-to-one call this device placed, and only until it is
    // answered. A group call rings nobody, and neither does joining one from a
    // link; before this every call that was not yet connected looped the tone,
    // so a person alone in a call they had just started heard a phone ringing,
    // and so did the device that had just answered.
    LaunchedEffect(phase, groupCall) {
        if (ringbackShouldPlay(rules, groupCall, phase)) {
            sounds.startLoop(CallSoundController.SoundName.RINGBACK_OUTGOING)
        } else {
            sounds.stopLoop(CallSoundController.SoundName.RINGBACK_OUTGOING)
        }
    }

    // The two short cues: in, and out. Each on the change only, which the
    // call's own tracker decides, so a screen brought back from the bar does
    // not play the connected sound again.
    val cueTracker = cues ?: callState.cues
    val remoteIds = participants.filter { !it.isLocal }.map { it.id }
    remember(cueTracker) {
        cueTracker.mount(callState.phase.value, callState.participants.value.filter { !it.isLocal }.map { it.id })
        true
    }
    LaunchedEffect(phase) {
        val cue = cueTracker.onPhase(phase)
        if (phase is CallUiState.CallPhase.Ended) sounds.stopAll()
        when (cue) {
            CallCue.JOIN -> sounds.playOnce(CallSoundController.SoundName.JOIN)
            CallCue.LEAVE -> sounds.playOnce(CallSoundController.SoundName.LEAVE)
            null -> Unit
        }
    }
    // Someone arriving or leaving, by id, so "Alice left, Bob joined" in the
    // same emission plays both. This device's own tile is not a peer.
    LaunchedEffect(remoteIds, phase) {
        for (cue in cueTracker.onRoster(remoteIds, phase is CallUiState.CallPhase.Connected)) {
            sounds.playOnce(
                when (cue) {
                    CallCue.JOIN -> CallSoundController.SoundName.JOIN
                    CallCue.LEAVE -> CallSoundController.SoundName.LEAVE
                },
            )
        }
    }

    // Who is speaking is read only while a screen is showing the tiles
    // (`docs/ux/group-call-grid.md` section 7.3); a group call only.
    DisposableEffect(callState, groupCall) {
        if (groupCall) callState.setSpeakingWatched(true)
        onDispose { if (groupCall) callState.setSpeakingWatched(false) }
    }

    // There is no branch on the phase any more. `docs/ux/calls.md` section 3
    // removed the preparation screen from every path — a voice call dials
    // immediately, a video call shows the camera check before the call exists
    // (the camera check, removed 2026-09-21 -- docs/ux/calls.md section 3.2), and
    // "accepting an incoming call never shows any preparation screen". So the
    // only thing this composable draws is the call.
    LakkoTheme(dark = true) {
        InCallView(
            state = callState,
            onExit = onExit,
            remoteName = remoteName,
            avatarSeed = avatarSeed,
            personKey = personKey,
            groupCall = groupCall,
            groupName = groupName,
            invite = invite,
            onCopyInviteLink = onCopyInviteLink,
            onInviteContact = onInviteContact,
            onEndForEveryone = onEndForEveryone,
            endedForEveryoneLine = endedForEveryoneLine,
            rules = rules,
            audio = audio,
            onSelectCamera = { name ->
                callState.devices.selectCamera(name)
                // A hosted call's own host follows the choice; a one-to-one
                // session is told here.
                session?.selectCamera(name)
            },
            onSwitchCamera = onSwitchCamera,
            onCollapse = onCollapse,
            connectedSinceMs = connectedSinceMs,
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
                                me.lakko.app.rethrowIfCancelled(cause)
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
                                me.lakko.app.rethrowIfCancelled(cause)
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
    personKey: ByteArray? = null,
    groupCall: Boolean = false,
    groupName: String? = null,
    /** See [CallScreen]'s parameters of the same names. */
    invite: InCallInvite? = null,
    onCopyInviteLink: () -> Unit = {},
    onInviteContact: (() -> Unit)? = null,
    onEndForEveryone: (() -> Unit)? = null,
    endedForEveryoneLine: String? = null,
    rules: GroupCallRules = SharedGroupCallRules,
    /** The call's audio route; `null` in a preview. */
    audio: CallAudioRouter? = null,
    /** A camera was chosen in the Settings sheet; `null` is the default one. */
    onSelectCamera: (String?) -> Unit = {},
    onSwitchCamera: (() -> Unit)? = null,
    /** See [CallScreen]'s parameter of the same name. */
    onCollapse: (() -> Unit)? = null,
    /** See [CallScreen]'s parameter of the same name — required for the same
     *  reason: an omitted moment is always a bug. */
    connectedSinceMs: Long?,
    onEndCall: () -> Unit = { state.endCall() },
    onSetMic: (Boolean) -> Unit = { state.setMic(it) },
    onSetCamera: (Boolean) -> Unit = { state.setCamera(it) },
) {
    val participants by state.participants.collectAsState()
    val phase by state.phase.collectAsState()
    val drawerOpen by state.participantListOpen.collectAsState()
    // docs/ux/calls.md section 4.1.4 — the call's chat, on a group call.
    val chatHandle by state.callChat.collectAsState()
    val chatModel = chatHandle?.model
    val chatOpen by (chatModel?.panelOpen ?: NO_CHAT_OPEN).collectAsState()
    val chatUnread by (chatModel?.unread ?: NO_CHAT_UNREAD).collectAsState()
    val chatPreview by (chatModel?.preview ?: NO_CHAT_PREVIEW).collectAsState()
    val e2ee by state.e2ee.collectAsState()
    val localVideoTrack by state.localVideoTrack.collectAsState()
    val remoteVideoTrack by state.remoteVideoTrack.collectAsState()
    val videoPublishState by state.videoPublishState.collectAsState()
    val hasMultipleCameras by state.hasMultipleCameras.collectAsState()
    val frontFacing by state.cameraFrontFacing.collectAsState()
    // ADR-0083 — the call's current screen share and the pictures of shares.
    val screenShare by state.screenShare.collectAsState()
    val screenTracks by state.remoteScreenVideoTracks.collectAsState()
    // docs/ux/calls.md section 5 — the digits are behind the chip, never on the
    // call. Closed on every new call because the state object is reset with the
    // screen, so a sheet can never open over a call it does not belong to.
    var encryptionSheetOpen by remember { mutableStateOf(false) }
    // docs/ux/calls.md section 4.1 — the Settings sheet: microphone, camera,
    // speaker.
    var settingsOpen by remember { mutableStateOf(false) }
    // docs/ux/call-links.md section 3 — the menu Participants opens on a call
    // that can hand out a link to itself.
    var participantsMenuOpen by remember { mutableStateOf(false) }
    // docs/ux/calls.md section 4.1.2 — the choice `End` opens for the person
    // who started a group call.
    var endMenuOpen by remember { mutableStateOf(false) }
    // The line under the status row that says where the link went, held for a
    // few seconds and then taken down ([inviteStatusHoldMs]).
    var inviteLineShown by remember { mutableStateOf(false) }
    LaunchedEffect(invite) {
        if (invite == null || invite is InCallInvite.Idle) {
            inviteLineShown = false
            return@LaunchedEffect
        }
        inviteLineShown = true
        val hold = inviteStatusHoldMs(invite) ?: return@LaunchedEffect
        delay(hold)
        inviteLineShown = false
    }
    val inviteStatus = invite?.takeIf { inviteLineShown }?.let { inviteStatusLine(it) }?.text()

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
        // docs/ux/screen-share.md section 5.1 — the other person's share is
        // the large picture, from its announcement on.
        screenSharePresent = screenShare?.isOwn == false,
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
        strings = LocalStrings.current,
    )
    val seed = avatarSeed?.takeIf { it.isNotBlank() } ?: displayName

    // Section 4: "The controls fade out after 5 seconds of a connected video
    // call and come back on a tap; on a voice call they never hide."
    var controlsVisible by remember { mutableStateOf(true) }
    // Alone means nobody else's tile is on the screen, so hiding the row
    // reveals nothing and only removes the way out.
    val mayHide = controlsMayAutoHide(stage, connected, alone = participants.none { !it.isLocal })
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

    var rootSize by remember { mutableStateOf(IntSize.Zero) }
    val density = LocalDensity.current
    val rootWidthDp = with(density) { rootSize.width.toDp() }
    val rootHeightDp = with(density) { rootSize.height.toDp() }

    // docs/ux/calls.md section 4.1 — one Participants control: the participant
    // list, and on a call that can hand out a link to itself the two invite
    // items.
    val participantsControl = participantsControl(
        group = group,
        canInvite = invite != null,
        othersPresent = remotes.isNotEmpty(),
    )
    val controls = callControlRow(
        micOn = local?.micOn != false,
        cameraOn = local?.camOn == true,
        cameraBusy = videoPublishState == CallUiState.VideoPublishState.Starting,
        ringing = phase is CallUiState.CallPhase.Calling,
        participants = participantsControl,
        participantsOpen = drawerOpen,
        chat = chatModel?.let { CallChatButton(open = chatOpen, unread = chatUnread) },
    )

    // What the control bar takes at the bottom of the screen, its margins and
    // the system inset included. The group tiles end above it: nothing is
    // drawn under the bar.
    var barHeightPx by remember { mutableStateOf(0) }
    val barHeight = with(density) { barHeightPx.toDp() }
    // Where each bar button's centre is, in the window root's coordinates,
    // and where this screen starts in them: a menu that opens from a button
    // sits above that button (`anchoredMenuLeft`), and follows it when the
    // bar re-fits or the screen rotates.
    val controlCentersX = remember { mutableStateMapOf<CallControlKind, Float>() }
    var rootLeftInWindowRoot by remember { mutableFloatStateOf(0f) }
    fun anchorX(kind: CallControlKind): Float? = controlCentersX[kind]?.minus(rootLeftInWindowRoot)
    // Room for the chevron at the top left and the layout switch at the top
    // right, so the status line is not drawn under either.
    val showsLayoutSwitch = group && remotes.isNotEmpty()
    val headerStartInset = if (onCollapse != null) 44.dp else 0.dp
    val headerEndInset = if (showsLayoutSwitch) 44.dp else 0.dp
    val qualityPopoverOpen by state.qualityPopoverOpen.collectAsState()

    // Section 4.5 — the call's own connect moment, resolved ONCE here so both
    // stage layouts count from the same instant and neither can reach for a
    // clock of its own. See [callClockOrigin] for what goes wrong when a view
    // supplies the moment.
    val stateConnectedSince by state.connectedSinceMs.collectAsState()
    val clockOrigin = callClockOrigin(
        callConnectedSinceMs = connectedSinceMs,
        viewStateConnectedSinceMs = stateConnectedSince,
    )

    // docs/ux/group-call-grid.md section 6.2: a press anywhere on the call
    // screen other than the tile whose controls are up hides them. The watch
    // is on the root, because the call area is only the tiles: the status
    // line, the chevron, the layout switch, the control bar, the drawer and
    // the margins are its siblings here, not its children.
    val tileControls = rememberTileControlsHost()

    // docs/ux/calls.md section 4.1.4 — at 900 dp and wider the chat is a
    // column at the end side and the tiles reflow into the width that is
    // left; below, a panel over the whole call screen.
    BoxWithConstraints(modifier = Modifier.fillMaxSize().background(LakkoDark.background)) {
    val chatColumn = callChatIsColumn(maxWidth.value)
    Row(modifier = Modifier.fillMaxSize()) {
    Box(
        modifier = Modifier
            .weight(1f)
            .fillMaxHeight()
            .background(LakkoDark.background)
            .onSizeChanged { rootSize = it }
            .onGloballyPositioned { rootLeftInWindowRoot = it.positionInRoot().x }
            .hidesTileControlsOnPressElsewhere(tileControls),
    ) {
        when (stage) {
            CallStage.PORTRAIT -> CallPortrait(
                state = state,
                phase = phase,
                name = displayName,
                seed = seed,
                personKey = personKey.takeIf { !groupCall },
                e2ee = e2ee,
                onEncryptionChip = { encryptionSheetOpen = true },
                clockOrigin = clockOrigin,
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
                        mirrored = onLocal && frontFacing,
                        scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
                    )
                }
                // docs/ux/call-backgrounds.md section 3 — nothing is sent
                // while a background gets ready: the avatar with a spinner.
                if (onLocal && backgroundStarting()) {
                    BackgroundStartingCover(modifier = Modifier.fillMaxSize(), avatarSize = 96.dp)
                }
                FloatingStatusHeader(
                    state = state,
                    phase = phase,
                    name = displayName,
                    e2ee = e2ee,
                    onEncryptionChip = { encryptionSheetOpen = true },
                    visible = controlsVisible,
                    clockOrigin = clockOrigin,
                    startInset = headerStartInset,
                    modifier = Modifier.align(Alignment.TopCenter),
                )
            }
            CallStage.SCREEN_SHARE -> {
                // docs/ux/screen-share.md section 5.1: the share is the large
                // picture, whole and zoomable; the other person's camera, or
                // their initials, is the small tile in the other corner.
                ScreenSharePicture(
                    track = screenTracks[CallUiState.ONE_TO_ONE_REMOTE_ID],
                    sharerName = displayName,
                    zoomable = true,
                    onTap = { controlsVisible = true },
                    modifier = Modifier.fillMaxSize(),
                )
                OtherPersonPip(
                    track = remoteVideoTrack.takeIf { soleRemote?.camOn == true },
                    name = displayName,
                    seed = seed,
                    personKey = personKey.takeIf { !groupCall },
                    modifier = Modifier
                        .align(Alignment.TopStart)
                        .windowInsetsPadding(WindowInsets.safeDrawing)
                        .padding(top = 110.dp, start = 16.dp),
                )
                FloatingStatusHeader(
                    state = state,
                    phase = phase,
                    name = displayName,
                    e2ee = e2ee,
                    onEncryptionChip = { encryptionSheetOpen = true },
                    visible = controlsVisible,
                    clockOrigin = clockOrigin,
                    startInset = headerStartInset,
                    modifier = Modifier.align(Alignment.TopCenter),
                )
            }
            CallStage.GRID -> {
                // docs/ux/group-call-grid.md section 4: the tiles fill the call
                // AREA, the space below the status line and above the control
                // bar, inside the side margins. A column rather than layers, so
                // the area is exactly what is left between the two.
                val sideMargin = if (rootWidthDp < 600.dp) 12.dp else 16.dp
                Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(bottom = barHeight),
                ) {
                    FloatingStatusHeader(
                        state = state,
                        phase = phase,
                        name = displayName,
                        e2ee = e2ee,
                        onEncryptionChip = { encryptionSheetOpen = true },
                        visible = true,
                        clockOrigin = clockOrigin,
                        groupCall = true,
                        // Section 4.1.2 — why the call ended, under
                        // `Call ended`, in the same place as the invite line.
                        inviteStatus = endedForEveryoneLine
                            ?.takeIf { phase is CallUiState.CallPhase.Ended }
                            ?: inviteStatus,
                        startInset = headerStartInset,
                        endInset = headerEndInset,
                    )
                    GroupCallArea(
                        state = state,
                        participants = participants,
                        rules = rules,
                        controls = tileControls,
                        modifier = Modifier
                            .weight(1f)
                            .fillMaxWidth()
                            .windowInsetsPadding(WindowInsets.safeDrawing.only(WindowInsetsSides.Horizontal))
                            .padding(horizontal = sideMargin),
                    )
                }
            }
        }

        // Section 4.5 — the way to put the call away, in the idiom the platform
        // already teaches: a chevron at the top left, beside the system back
        // that means the same thing. It does NOT fade with the controls: the
        // controls fade to reveal the video, and a way out that disappears is
        // the thing this whole section exists to stop happening.
        if (onCollapse != null) {
            CollapseCallButton(
                onClick = onCollapse,
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .windowInsetsPadding(WindowInsets.safeDrawing)
                    .padding(start = 8.dp, top = 8.dp),
            )
        }

        // docs/ux/group-call-grid.md section 3 — the layout switch, the
        // chevron's twin at the top right. Absent while the person is alone:
        // there is nobody to put in a strip.
        if (showsLayoutSwitch) {
            GroupLayoutSwitch(
                state = state,
                participants = participants,
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .windowInsetsPadding(WindowInsets.safeDrawing)
                    .padding(end = 8.dp, top = 8.dp),
            )
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
                mirrored = frontFacing,
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
        androidx.compose.animation.AnimatedVisibility(
            visible = controlsVisible,
            enter = fadeIn(),
            exit = fadeOut(),
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .onSizeChanged { barHeightPx = it.height }
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(
                    horizontal = GROUP_CONTROL_SIDE_MARGIN_DP.dp,
                    vertical = when {
                        stage == CallStage.PORTRAIT -> 72.dp
                        group -> 12.dp
                        else -> 16.dp
                    },
                ),
        ) {
            // docs/ux/calls.md section 4.1 — the whole set is on the screen, in
            // one row, on every call. A group call's row has no words under the
            // circles on a phone and is sized from the width, so it never wraps
            // and never clips; there is no overflow menu.
            CallControlBar(
                controls = controls,
                group = group,
                screenHeightDp = rootHeightDp.value,
                onButtonCenterX = { kind, x ->
                    if (controlCentersX[kind] != x) controlCentersX[kind] = x
                },
                onControl = { kind ->
                    when (kind) {
                        CallControlKind.MICROPHONE -> onSetMic(local?.micOn != true)
                        CallControlKind.CAMERA -> onSetCamera(local?.camOn != true)
                        CallControlKind.PARTICIPANTS -> when (participantsControl) {
                            ParticipantsControl.MENU -> participantsMenuOpen = !participantsMenuOpen
                            ParticipantsControl.LIST -> state.setParticipantListOpen(!drawerOpen)
                            ParticipantsControl.ABSENT -> Unit
                        }
                        CallControlKind.CHAT -> chatModel?.togglePanel()
                        CallControlKind.SETTINGS -> settingsOpen = true
                        // Section 4.1.2 — for the starter, while anyone else
                        // is in the call, the first press opens the choice and
                        // never ends the call.
                        CallControlKind.END ->
                            if (endOpensMenu(onEndForEveryone != null, remotes.size)) {
                                endMenuOpen = !endMenuOpen
                            } else {
                                onEndCall()
                            }
                    }
                },
            )
        }

        // docs/ux/call-backgrounds.md section 3 — a background that stopped
        // working turns the camera off through the camera button's path and
        // says so here for a few seconds.
        BackgroundFailureLine(
            cameraOn = local?.camOn == true,
            onCameraOff = { onSetCamera(false) },
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(bottom = barHeight + 24.dp),
        )

        // docs/ux/screen-share.md section 5.3 — `Anna started sharing their
        // screen` and `Anna stopped sharing`, polite, once each.
        val unnamed = LocalStrings.current["call.participant.unnamed"]
        ScreenShareAnnouncer(state) { peerId ->
            if (groupCall) {
                participants.firstOrNull { it.id == me.lakko.app.calls.remoteParticipantId(peerId) }
                    ?.displayName?.takeIf { it.isNotEmpty() } ?: unnamed
            } else {
                displayName
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

        // docs/ux/call-links.md section 3 — Participants opens a menu of the
        // app's own above the bar, never the system's share sheet.
        if (participantsMenuOpen && participantsControl == ParticipantsControl.MENU) {
            CallParticipantsMenu(
                items = participantsMenuItems(
                    othersPresent = remotes.isNotEmpty(),
                    // No handler means the session is a guest's (or there
                    // is no session to send from): the link alone.
                    guest = onInviteContact == null,
                ),
                onItem = { item ->
                    participantsMenuOpen = false
                    when (item) {
                        ParticipantsMenuItem.PARTICIPANTS -> state.setParticipantListOpen(true)
                        ParticipantsMenuItem.COPY_LINK -> onCopyInviteLink()
                        ParticipantsMenuItem.INVITE_CONTACTS -> onInviteContact?.invoke()
                    }
                },
                onDismiss = { participantsMenuOpen = false },
                bottomInset = barHeight,
                anchorCenterX = anchorX(CallControlKind.PARTICIPANTS),
                areaWidthPx = rootSize.width,
            )
        }

        // docs/ux/calls.md section 4.1.2 — `End` for the person who started a
        // group call: leave, or end it for everyone. A menu of the app's own.
        if (endMenuOpen && onEndForEveryone != null) {
            CallEndMenu(
                onLeave = {
                    endMenuOpen = false
                    onEndCall()
                },
                onEndForEveryone = {
                    endMenuOpen = false
                    onEndForEveryone()
                },
                onDismiss = { endMenuOpen = false },
                bottomInset = barHeight,
                anchorCenterX = anchorX(CallControlKind.END),
                areaWidthPx = rootSize.width,
            )
        }

        // docs/ux/calls.md section 4.1 — the Settings sheet: the app's own
        // radio lists of the devices the call can be put on.
        if (settingsOpen) {
            val strings = LocalStrings.current
            val context = LocalContext.current
            val cameras = remember(context) { CameraVideoCapture.cameras(context).map { it.name } }
            val inputs by (audio?.inputs ?: NO_INPUTS).collectAsState()
            val outputs by (audio?.outputs ?: NO_OUTPUTS).collectAsState()
            val chosenCamera by state.devices.camera.collectAsState()
            val chosenOutput by state.devices.audioOutput.collectAsState()
            val chosenInput by state.devices.audioInput.collectAsState()
            remember(context) { me.lakko.app.calls.NoiseSuppressionSetting.attach(context) }
            val noiseOn by me.lakko.app.calls.NoiseSuppressionSetting.on.collectAsState()
            // Section 4.1 — `Your name in this call`, on a group or link call.
            val callNameControl by state.callNameControl.collectAsState()
            val ownCallName by state.ownCallName.collectAsState()
            val callNameDefault by state.callNameDefault.collectAsState()
            CallSettingsSheet(
                noiseSuppressionOn = noiseOn,
                onNoiseSuppressionChange = { me.lakko.app.calls.NoiseSuppressionSetting.set(it) },
                callNameControl = callNameControl,
                ownCallName = ownCallName,
                callNameDefault = callNameDefault,
                cameraOn = local?.camOn == true,
                preferredCamera = chosenCamera,
                onTurnCameraOnWithoutBackground = {
                    // docs/ux/call-backgrounds.md section 3: no background,
                    // then the camera on through the camera button's path.
                    me.lakko.app.calls.CameraBackground.select(
                        context,
                        me.lakko.app.calls.BackgroundSelection.None,
                    )
                    if (local?.camOn != true) onSetCamera(true)
                },
                sections = callSettingsSections(
                    devices = callSettingsSheetDevices(inputs, cameras, outputs, strings),
                    selection = SettingsSelection(
                        audioInput = chosenInput?.toString(),
                        videoInput = chosenCamera,
                        audioOutput = chosenOutput?.toString(),
                    ),
                    canSelectAudioOutput = true,
                    strings = strings,
                ),
                onChoose = { kind, id ->
                    when (kind) {
                        SettingsSectionKind.MICROPHONE -> {
                            val input = id?.toIntOrNull()
                            state.devices.selectAudioInput(input)
                            audio?.selectInput(input)
                        }
                        SettingsSectionKind.CAMERA -> onSelectCamera(id)
                        SettingsSectionKind.SPEAKER -> {
                            val output = id?.toIntOrNull()
                            state.devices.selectAudioOutput(output)
                            audio?.selectOutput(output)
                        }
                    }
                },
                onDismiss = { settingsOpen = false },
            )
        }

        // docs/ux/calls.md section 5 — the safety number, only when asked for.
        if (encryptionSheetOpen) {
            CallEncryptionSheet(
                safetyNumber = e2ee.shortFingerprint,
                otherName = remoteName,
                groupCall = groupCall,
                othersInCall = remotes.size,
                onDismiss = { encryptionSheetOpen = false },
            )
        }

        // docs/ux/calls.md section 4.1.4 — a message from someone else, for 4
        // seconds, one line above the control bar while the panel is closed.
        // Not while the bar is faded, nor over a sheet or a list.
        val shownPreview = chatPreview
        if (chatModel != null && shownPreview != null && callChatPreviewShown(
                preview = shownPreview,
                panelOpen = chatOpen,
                controlsVisible = controlsVisible,
                overlayOpen = settingsOpen || drawerOpen || participantsMenuOpen || endMenuOpen || encryptionSheetOpen,
            )
        ) {
            CallChatPreviewLine(
                preview = shownPreview,
                model = chatModel,
                participants = participants,
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = barHeight + 4.dp, start = 16.dp, end = 16.dp),
            )
        }

        // The full panel (under 900 dp): over the whole call screen, sliding
        // up from the bottom. The call keeps running behind it.
        val panelHandle = chatHandle
        androidx.compose.animation.AnimatedVisibility(
            visible = panelHandle != null && chatOpen && !chatColumn,
            enter = slideInVertically { it },
            exit = slideOutVertically { it },
            modifier = Modifier.fillMaxSize(),
        ) {
            if (panelHandle != null) {
                CallChatPanel(
                    handle = panelHandle,
                    participants = participants,
                    column = false,
                    micControl = callBarMicControl(
                        micOn = local?.micOn != false,
                        ringing = phase is CallUiState.CallPhase.Calling,
                    ),
                    onToggleMic = { onSetMic(local?.micOn != true) },
                    onClose = { panelHandle.model.closePanel() },
                    modifier = Modifier.fillMaxSize(),
                )
            }
        }
    }
    // The column (900 dp and wider): the control row stays visible beside it.
    val columnHandle = chatHandle
    if (columnHandle != null && chatOpen && chatColumn) {
        CallChatPanel(
            handle = columnHandle,
            participants = participants,
            column = true,
            micControl = null,
            onToggleMic = {},
            onClose = { columnHandle.model.closePanel() },
            modifier = Modifier.width(CALL_CHAT_COLUMN_WIDTH_DP.dp).fillMaxHeight(),
        )
    }
    }
    }
}

/**
 * `docs/ux/calls.md` section 4.5 — put the call away without ending it.
 *
 * A chevron and not a back arrow: back is for leaving a place, and the call is
 * not being left. The same glyph the platform's own sheets and media players use
 * for "minimise this, it keeps running".
 */
@Composable
private fun CollapseCallButton(onClick: () -> Unit, modifier: Modifier = Modifier) {
    val collapseLabel = t("call.collapse_a11y")
    Box(
        modifier = modifier
            .size(44.dp)
            .clip(CircleShape)
            .background(LakkoDark.background.copy(alpha = 0.45f))
            .clickable(onClick = onClick)
            .semantics(mergeDescendants = true) {
                contentDescription = collapseLabel
                role = Role.Button
            },
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            imageVector = Icons.Filled.KeyboardArrowDown,
            contentDescription = null,
            tint = LakkoDark.text,
            modifier = Modifier.size(26.dp),
        )
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
    personKey: ByteArray?,
    e2ee: CallUiState.E2eeState,
    onEncryptionChip: () -> Unit,
    /** The CALL's connect moment — see [callClockOrigin]. */
    clockOrigin: Long?,
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
            LakkoAvatar(name = name, seed = seed, size = 132.dp, personKey = personKey)
            Spacer(Modifier.height(24.dp))
            CallStatusLine(
                state = state,
                phase = phase,
                remoteName = name,
                e2ee = e2ee,
                onEncryptionChip = onEncryptionChip,
                portrait = true,
                clockOrigin = clockOrigin,
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
    /** The CALL's connect moment — see [callClockOrigin]. */
    clockOrigin: Long?,
    modifier: Modifier = Modifier,
    /** See [CallStatusLine]'s parameters of the same names. */
    groupCall: Boolean = false,
    inviteStatus: String? = null,
    /** Room kept clear at each end for a button that sits in that corner. */
    startInset: Dp = 0.dp,
    endInset: Dp = 0.dp,
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
                .windowInsetsPadding(WindowInsets.safeDrawing.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal))
                .padding(start = 20.dp + startInset, end = 20.dp + endInset, top = 12.dp, bottom = 12.dp),
        ) {
            CallStatusLine(
                state = state,
                phase = phase,
                remoteName = name,
                e2ee = e2ee,
                onEncryptionChip = onEncryptionChip,
                portrait = false,
                clockOrigin = clockOrigin,
                groupCall = groupCall,
                inviteStatus = inviteStatus,
            )
        }
    }
}

/**
 * The self-view of `docs/ux/calls.md` section 4: a small rounded rectangle in a
 * corner, draggable, about a quarter of the width, carrying the flip-camera
 * button of section 4.1 "where the thing it changes is visible".
 */
/**
 * The other person's small tile while their share is the large picture
 * (`docs/ux/screen-share.md` section 5.1): their camera, never mirrored, or
 * their initials when it is off.
 */
@Composable
private fun OtherPersonPip(
    track: VideoTrack?,
    name: String,
    seed: String,
    personKey: ByteArray?,
    modifier: Modifier = Modifier,
) {
    val egl = remember { WebRtcPeerConnection.sharedEglContext() }
    val tint = remember(seed) { avatarTintFor(seed, dark = true) }
    Box(
        modifier = modifier
            .size(width = 104.dp, height = 152.dp)
            .clip(RoundedCornerShape(16.dp))
            .background(tint.background)
            .clearAndSetSemantics { contentDescription = name },
        contentAlignment = Alignment.Center,
    ) {
        if (track != null && egl != null) {
            LocalVideoPreviewView(
                track = track,
                eglContext = egl,
                modifier = Modifier.fillMaxSize(),
                mirrored = false,
                scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
            )
        } else {
            LakkoAvatar(name = name, seed = seed, size = 56.dp, personKey = personKey)
        }
    }
}

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
        }
        if (track != null && backgroundStarting()) {
            BackgroundStartingCover(modifier = Modifier.fillMaxSize(), avatarSize = 48.dp)
        } else if (track == null && starting) {
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
    val flipLabel = t("call.self.flip_a11y")
    Box(
        modifier = modifier
            .size(36.dp)
            .clip(CircleShape)
            .background(LakkoDark.background.copy(alpha = 0.6f))
            .clickable(onClick = onClick)
            .semantics { contentDescription = flipLabel; role = Role.Button },
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
    /**
     * The moment the CALL connected, already resolved by [callClockOrigin].
     *
     * This line counts from there and reads no clock of its own except `now`.
     * It used to take its origin from `System.currentTimeMillis()` at the moment
     * it first observed the connect, which is the same thing for a screen that
     * is up for the whole call and wrong once the call can be put away and
     * brought back (section 4.5): expanding rebuilds this composable, so a
     * twelve-minute call came back reading `0:00`.
     */
    clockOrigin: Long?,
    modifier: Modifier = Modifier,
    /** A group call or a link call: its line reads `Connecting...` while it
     *  starts, never `Calling...` (section 3.4: it rings nobody). */
    groupCall: Boolean = false,
    /** `docs/ux/call-links.md` section 3 — one line under the status row
     *  saying what `Invite` did, or `null`. A plain line rather than a banner,
     *  so it reads like the rest of the status block. */
    inviteStatus: String? = null,
) {
    val connected = phase is CallUiState.CallPhase.Connected
    val ended = phase is CallUiState.CallPhase.Ended
    // A one-second tick against the CALL's origin. The tick STOPS when the call
    // leaves `Connected` — a finished call must not keep counting while it waits
    // to dismiss itself — but the last value is KEPT once the call has ended,
    // because section 4.1 asks the ended state for the duration and there is
    // nowhere else to get it from.
    var elapsedSecs by remember { mutableStateOf<Long?>(null) }
    LaunchedEffect(connected, ended, clockOrigin) {
        if (!connected) {
            if (!ended) elapsedSecs = null
            return@LaunchedEffect
        }
        while (true) {
            // First value before the first delay, so a screen brought back
            // mid-call shows the real number immediately rather than a second
            // of the last one it remembered.
            elapsedSecs = callElapsedSecs(clockOrigin, System.currentTimeMillis())
            delay(1_000)
        }
    }

    // Already decided by [callScreenTitle] at the stage; passed through whole so
    // the line cannot re-derive a different answer from the roster.
    val strings = LocalStrings.current
    val title = remoteName?.trim()?.takeIf { it.isNotEmpty() } ?: strings["chat.call.voice"]
    val status = callStatusTextFor(
        groupCall = groupCall,
        phase = phase,
        elapsedSecs = elapsedSecs,
        strings = strings,
    )
    val spoken = callStatusSpokenLabel(title, status, elapsedSecs, strings)
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
        if (inviteStatus != null) {
            Spacer(Modifier.height(4.dp))
            Text(
                text = inviteStatus,
                color = LakkoDark.textMuted,
                fontSize = 13.sp,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
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
    val chipSpoken = t("call.encryption.chip_a11y")
    Row(
        modifier = Modifier
            .clip(RoundedCornerShape(50))
            .background(LakkoDark.sunken, RoundedCornerShape(50))
            .border(1.dp, LakkoDark.border, RoundedCornerShape(50))
            .clickable(onClick = onClick)
            .semantics(mergeDescendants = true) {
                contentDescription = chipSpoken
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
            text = CALL_ENCRYPTION_CHIP_LABEL.text(),
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
    groupCall: Boolean,
    othersInCall: Int,
    onDismiss: () -> Unit,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val clipboard = LocalClipboardManager.current
    val groups = remember(safetyNumber) { safetyNumberGroups(safetyNumber) }
    val strings = LocalStrings.current
    val spoken = remember(safetyNumber, strings) { safetyNumberSpokenLabel(safetyNumber, strings) }
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
                CALL_ENCRYPTION_SHEET_TITLE.text(),
                color = LakkoDark.text,
                fontSize = 20.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                callEncryptionSentence(otherName, groupCall, othersInCall).text(),
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
                callEncryptionHint(groupCall, othersInCall).text(),
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
                    Text(t(if (copied) "common.copied" else "common.copy"), color = LakkoDark.text)
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
                    Text(t("common.done"))
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
                        t("call.quality.title", mapOf("tier" to t("call.quality.tier.${tier.name.lowercase()}"))),
                        color = LakkoDark.text,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        when (detail.connectionType) {
                            CallUiState.ConnectionDetail.ConnectionType.Direct -> t("call.quality.direct")
                            CallUiState.ConnectionDetail.ConnectionType.Relay -> t("call.quality.relay")
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
                val strings = LocalStrings.current
                val locale = remember(strings.language) { java.util.Locale.forLanguageTag(strings.language) }
                val rtt = detail.rttMs?.let {
                    strings.format("call.quality.ms", mapOf("value" to me.lakko.app.l10n.DeviceFormats.integer(it.toLong(), locale, me.lakko.app.l10n.DeviceFormats.region())))
                } ?: "—"
                Text(
                    strings.format("common.fact_pair", mapOf("first" to strings["call.quality.round_trip"], "second" to rtt)),
                    color = LakkoDark.text,
                    fontSize = 11.sp,
                )
                Text(
                    strings.format("common.fact_pair", mapOf("first" to strings["call.quality.down"], "second" to formatBps(detail.downBps, strings, locale))),
                    color = LakkoDark.text,
                    fontSize = 11.sp,
                )
                Text(
                    strings.format("common.fact_pair", mapOf("first" to strings["call.quality.up"], "second" to formatBps(detail.upBps, strings, locale))),
                    color = LakkoDark.text,
                    fontSize = 11.sp,
                )
            }
        }
    }
}

/** A bitrate with its unit as its own message; the number formatted for the interface language. */
private fun formatBps(bps: Int?, strings: Strings, locale: java.util.Locale): String {
    if (bps == null) return "—"
    // Separators from the device's region (localization.md section 5).
    val region = me.lakko.app.l10n.DeviceFormats.region()
    if (bps >= 1_000_000) {
        return strings.format(
            "call.quality.mbps",
            mapOf("value" to me.lakko.app.l10n.DeviceFormats.decimal(bps / 1_000_000.0, 1, locale, region)),
        )
    }
    if (bps >= 1_000) {
        return strings.format(
            "call.quality.kbps",
            mapOf("value" to me.lakko.app.l10n.DeviceFormats.integer((bps / 1_000).toLong(), locale, region)),
        )
    }
    return strings.format("call.quality.bps", mapOf("value" to me.lakko.app.l10n.DeviceFormats.integer(bps.toLong(), locale, region)))
}

/**
 * The name on a participant's tile and row (ADR-0061): the name the call
 * carries, else `call.participant.you` for this device and
 * `call.participant.unnamed` for someone this device cannot name.
 */
@Composable
private fun participantName(participant: CallUiState.Participant): String =
    participant.displayName.takeIf { it.isNotEmpty() }
        ?: t(if (participant.isLocal) "call.participant.you" else "call.participant.unnamed")

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
                t("call.participants.title", mapOf("count" to participants.size.toString())),
                color = LakkoDark.text,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            IconButton(onClick = { state.setParticipantListOpen(false) }) {
                Icon(
                    Icons.Filled.Close,
                    contentDescription = t("call.participants.close_a11y"),
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
                    // docs/ux/group-call-grid.md section 7.2 — a connection is
                    // shown only when it is fair or worse, never a constant.
                    if (connectionIsWorthShowing(p.quality)) {
                        Box(
                            modifier = Modifier
                                .size(8.dp)
                                .background(p.quality.colour(), CircleShape),
                        )
                    }
                    Text(
                        text = participantName(p),
                        color = LakkoDark.text,
                        modifier = Modifier.weight(1f),
                    )
                    if (!p.micOn) {
                        Icon(
                            Icons.Filled.MicOff,
                            contentDescription = t("call.participant.muted_a11y"),
                            tint = LakkoDark.danger,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                }
                Divider(color = LakkoDark.border)
            }
        }
    }
}

/** What the Settings sheet reads where there is no audio route to ask. */
private val NO_INPUTS: StateFlow<List<CallAudioInput>> = MutableStateFlow(emptyList())
private val NO_CHAT_OPEN: StateFlow<Boolean> = MutableStateFlow(false)
private val NO_CHAT_UNREAD: StateFlow<Int> = MutableStateFlow(0)
private val NO_CHAT_PREVIEW: StateFlow<CallChatPreview?> = MutableStateFlow(null)
private val NO_OUTPUTS: StateFlow<List<CallAudioOutput>> = MutableStateFlow(emptyList())
