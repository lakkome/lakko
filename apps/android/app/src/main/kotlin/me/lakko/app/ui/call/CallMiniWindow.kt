// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.VectorConverter
import androidx.compose.animation.core.spring
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.absoluteOffset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
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
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import me.lakko.app.calls.AppCallHost
import me.lakko.app.calls.AvRole
import me.lakko.app.calls.CallBarModel
import me.lakko.app.calls.CallKind
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.GroupCallHost
import me.lakko.app.calls.LinkCallHost
import me.lakko.app.calls.MINI_WINDOW_FOOTER_DP
import me.lakko.app.calls.MINI_WINDOW_HEADER_DP
import me.lakko.app.calls.MINI_WINDOW_MARGIN_DP
import me.lakko.app.calls.MINI_WINDOW_TUCK_STRIP_HEIGHT_DP
import me.lakko.app.calls.MINI_WINDOW_TUCK_STRIP_WIDTH_DP
import me.lakko.app.calls.MiniWindowArea
import me.lakko.app.calls.MiniWindowCorner
import me.lakko.app.calls.MiniWindowPlacement
import me.lakko.app.calls.MiniWindowPoint
import me.lakko.app.calls.MiniWindowRest
import me.lakko.app.calls.MiniWindowSize
import me.lakko.app.calls.OneToOneCallHost
import me.lakko.app.calls.SharedGroupCallRules
import me.lakko.app.calls.WebRtcPeerConnection
import me.lakko.app.calls.askedQuality
import me.lakko.app.calls.callBarMicControl
import me.lakko.app.calls.callBarPhaseOf
import me.lakko.app.calls.callBarStatusText
import me.lakko.app.calls.callScreenTitle
import me.lakko.app.calls.callStatusText
import me.lakko.app.calls.groupCallBarPhaseOf
import me.lakko.app.calls.heavyTrackForGiven
import me.lakko.app.calls.miniWindowCornerPosition
import me.lakko.app.calls.miniWindowInsetAtStart
import me.lakko.app.calls.miniWindowPeopleBeyond
import me.lakko.app.calls.miniWindowRelease
import me.lakko.app.calls.miniWindowSize
import me.lakko.app.calls.miniWindowStripPulledOut
import me.lakko.app.calls.miniWindowTuckAllowed
import me.lakko.app.calls.miniWindowWidthPx
import me.lakko.app.calls.oneToOneRemoteMedia
import me.lakko.app.calls.peerIdOfParticipant
import me.lakko.app.calls.sharerParticipantId
import me.lakko.app.calls.showsCallEncryptionChip
import me.lakko.app.calls.spokenCallDuration
import me.lakko.app.calls.stagePerson
import me.lakko.app.calls.tileQualityOf
import me.lakko.app.calls.withShareTile
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.joinList
import me.lakko.app.l10n.text
import me.lakko.app.ui.chat.initialsOf
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.ui.theme.avatarTintFor
import org.webrtc.RendererCommon
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.CallBarPhase
import uniffi.lakko_ffi_uniffi.CallBarStatus
import uniffi.lakko_ffi_uniffi.TileVideoQuality
import uniffi.lakko_ffi_uniffi.callBarStatus
import uniffi.lakko_ffi_uniffi.callBarVisible
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * `docs/ux/calls.md` section 4.5 — the live call, put away into one small
 * window that floats over the app, and the same picture in the system's own
 * picture-in-picture window when the app leaves the screen (section 4.5.4).
 *
 * It replaced a bar across the top of the app (maintainer decision of
 * 2026-11-09: the window always, voice calls too). The bar kept the call
 * reachable but hid every face; the window keeps the picture.
 *
 * ## Where the decisions live
 *
 * Whether the call can be put away at all and the one thing its status line
 * says are the shared `call_bar` rule (`crates/lakko-ui-rules/src/call_bar.rs`)
 * over UniFFI, unchanged from the bar. Who is in the picture is the call
 * screen's own stage rule (`call_stage`), and which picture of them the window
 * asks the SFU for is `tile_video_quality` at the window's width. Where the
 * window rests is this client's own (section 4.5.2) and lives, pure, in
 * [me.lakko.app.calls.miniWindowRelease] and its neighbours.
 *
 * ## There is no End on the window
 *
 * Section 4.5.1: ending is the one irreversible control in a call, and a
 * small window a thumb crosses on the way to a chat is the wrong place for
 * it. Coming back costs one tap.
 */

/** What the shown person's picture is. */
internal sealed class MiniWindowPicture {
    /** A person in a group or link call sending their camera: the tracks the
     *  call holds of them, and what the SFU said it gives. */
    data class GroupCamera(
        val participantId: String,
        val main: VideoTrack,
        val middle: VideoTrack?,
        val light: VideoTrack?,
        val given: TileVideoQuality?,
    ) : MiniWindowPicture()

    /** One video track drawn whole (the other person's camera in a one-to-one
     *  call, or this device's own camera when it is alone). */
    data class Video(val track: VideoTrack, val mirrored: Boolean) : MiniWindowPicture()

    /** A screen being shared. */
    data class Share(val track: VideoTrack?, val sharerName: String) : MiniWindowPicture()

    /** A camera that is on and has not delivered a picture yet. */
    data object Waiting : MiniWindowPicture()

    /** The person's avatar on the call surface. [ownPerson] draws this
     *  device's owner's own photo. */
    data class Avatar(
        val name: String,
        val seed: String,
        val personKey: ByteArray?,
        val ownPerson: Boolean,
    ) : MiniWindowPicture()
}

/** This device's own camera, drawn as the inset (section 4.5.1). */
internal data class MiniWindowOwnVideo(val track: VideoTrack, val mirrored: Boolean)

/**
 * Everything the window and the system picture-in-picture window draw, for
 * the call that holds the one slot, or `null` when there is nothing to put
 * away: no call, a call only ringing in, a call that is over.
 *
 * @param bar the status line, the lock, the mute button and the call's label
 *   (spoken, never printed), decided exactly as the bar decided them.
 * @param collapsed the call has been put away; the window is shown only then.
 * @param groupUi the call's state for a group or link call, where the window
 *   says which picture of the shown person it draws; `null` for a one-to-one.
 */
internal data class CallMiniWindowModel(
    val bar: CallBarModel,
    val collapsed: Boolean,
    val shownName: String,
    val peopleBeyond: Int,
    val picture: MiniWindowPicture,
    val ownInset: MiniWindowOwnVideo?,
    val screenShare: Boolean,
    val people: Int,
    val groupUi: CallUiState?,
)

/**
 * Resolve the window's model from whichever call holds the slot.
 *
 * @param nameForOneToOne the other person's resolved name for a 1x1 chat.
 * @param keyForOneToOne the other person's signature key for a 1x1 chat
 *   (ADR-0084), so the avatar is their profile photo.
 * @param groupNameFor the group's own name (ADR-0051), or `null`.
 */
@Composable
internal fun rememberCallMiniWindow(
    oneToOne: OneToOneCallHost<CallSession>,
    group: GroupCallHost,
    link: LinkCallHost,
    nameForOneToOne: (groupIdHex: String) -> String?,
    keyForOneToOne: (groupIdHex: String) -> ByteArray?,
    groupNameFor: (groupIdHex: String) -> String?,
): CallMiniWindowModel? {
    val occupant by AppCallHost.occupant.collectAsStateWithLifecycle()
    return when (occupant) {
        null -> null
        CallKind.ONE_TO_ONE -> oneToOneMiniWindow(oneToOne, nameForOneToOne, keyForOneToOne)
        CallKind.GROUP -> hostedMiniWindow(
            ui = group.ui,
            collapsedFlow = group.collapsed,
            connectedSinceFlow = group.connectedSinceMs,
            groupName = group.slot.collectAsStateWithLifecycle().value?.groupIdHex?.let(groupNameFor),
        )
        CallKind.LINK -> hostedMiniWindow(
            ui = link.ui,
            collapsedFlow = link.collapsed,
            connectedSinceFlow = link.connectedSinceMs,
            groupName = null,
        )
    }
}

@Composable
private fun oneToOneMiniWindow(
    host: OneToOneCallHost<CallSession>,
    nameForOneToOne: (groupIdHex: String) -> String?,
    keyForOneToOne: (groupIdHex: String) -> ByteArray?,
): CallMiniWindowModel? {
    val call = host.active.collectAsStateWithLifecycle().value ?: return null
    val session = call.session
    val state by session.state.collectAsStateWithLifecycle()
    val collapsed by host.collapsed.collectAsStateWithLifecycle()
    val connectedSince by host.connectedSinceMs.collectAsStateWithLifecycle()
    val micOn by session.micEnabled.collectAsStateWithLifecycle()
    val cameraOn by session.cameraEnabled.collectAsStateWithLifecycle()
    val e2eeActive by session.e2eeActive.collectAsStateWithLifecycle()
    val safetyNumber by session.safetyNumber.collectAsStateWithLifecycle()
    val localTrack by session.localVideoTrack.collectAsStateWithLifecycle()
    val remoteTrack by session.remoteVideoTrack.collectAsStateWithLifecycle()
    val remoteCameraOn by session.remoteCameraOn.collectAsStateWithLifecycle()
    val remoteMicOn by session.remoteMicOn.collectAsStateWithLifecycle()
    val frontFacing by session.cameraIsFrontFacing.collectAsStateWithLifecycle()
    val share by session.screenShare.collectAsStateWithLifecycle()
    val screenTrack by session.remoteScreenTrack.collectAsStateWithLifecycle()
    val strings = LocalStrings.current
    val peerName = nameForOneToOne(call.groupIdHex)
    val label = callScreenTitle(groupName = null, peerName = peerName, groupCall = false, strings = strings)
    val bar = callBarModel(
        who = label,
        phase = callBarPhaseOf(state, accepted = call.accepted),
        connectedSinceMs = connectedSince,
        showsLock = showsCallEncryptionChip(e2eeActive, safetyNumber),
        cameraSending = cameraOn,
        micOn = micOn,
    ) ?: return null
    // Section 4.5.1: in a one-to-one call the picture is the other person —
    // their share while they share a screen, else their camera while it
    // sends, else their avatar, exactly as their tile draws it. An outgoing
    // call shows the person called.
    val screenShare = share?.isOwn == false
    val remoteMedia = oneToOneRemoteMedia(
        remoteHasVideo = remoteTrack != null,
        remoteCameraOn = remoteCameraOn,
        remoteMicOn = remoteMicOn,
    )
    val picture = when {
        screenShare -> MiniWindowPicture.Share(screenTrack, label)
        remoteMedia.cameraOn && remoteTrack != null -> MiniWindowPicture.Video(remoteTrack ?: return null, mirrored = false)
        else -> MiniWindowPicture.Avatar(
            name = label,
            seed = call.groupIdHex,
            personKey = keyForOneToOne(call.groupIdHex),
            ownPerson = false,
        )
    }
    return CallMiniWindowModel(
        bar = bar,
        collapsed = collapsed,
        shownName = label,
        peopleBeyond = 0,
        picture = picture,
        ownInset = localTrack?.takeIf { cameraOn }?.let { MiniWindowOwnVideo(it, mirrored = frontFacing) },
        screenShare = screenShare,
        people = 2,
        groupUi = null,
    )
}

/** The group (SFU) call and the call-link call: both driven through a
 *  host-owned [CallUiState], so both read the same way. */
@Composable
private fun hostedMiniWindow(
    ui: CallUiState,
    collapsedFlow: StateFlow<Boolean>,
    connectedSinceFlow: StateFlow<Long?>,
    groupName: String?,
): CallMiniWindowModel? {
    val phase by ui.phase.collectAsStateWithLifecycle()
    val collapsed by collapsedFlow.collectAsStateWithLifecycle()
    val connectedSince by connectedSinceFlow.collectAsStateWithLifecycle()
    val participants by ui.participants.collectAsStateWithLifecycle()
    val e2ee by ui.e2ee.collectAsStateWithLifecycle()
    val pinnedId by ui.pinnedId.collectAsStateWithLifecycle()
    val speech by ui.speech.collectAsStateWithLifecycle()
    val share by ui.screenShare.collectAsStateWithLifecycle()
    val localTrack by ui.localVideoTrack.collectAsStateWithLifecycle()
    val frontFacing by ui.cameraFrontFacing.collectAsStateWithLifecycle()
    val mains by ui.remoteVideoTracks.collectAsStateWithLifecycle()
    val middles by ui.remoteMiddleVideoTracks.collectAsStateWithLifecycle()
    val lights by ui.remoteLightVideoTracks.collectAsStateWithLifecycle()
    val screens by ui.remoteScreenVideoTracks.collectAsStateWithLifecycle()
    val given by ui.remoteVideoGiven.collectAsStateWithLifecycle()
    val strings = LocalStrings.current
    val local = participants.firstOrNull { it.isLocal }
    val bar = callBarModel(
        who = callScreenTitle(groupName = groupName, peerName = null, groupCall = true, strings = strings),
        phase = groupCallBarPhaseOf(phase),
        connectedSinceMs = connectedSince,
        showsLock = showsCallEncryptionChip(e2ee.active, e2ee.shortFingerprint),
        cameraSending = local?.camOn == true,
        micOn = local?.micOn != false,
    ) ?: return null
    // Section 4.5.1: the person the call screen would put on the stage, by
    // the same rule, with a share counted as a tile exactly as the grid does.
    val tiles = remember(participants, share) { withShareTile(participants, share) }
    val shown = stagePerson(SharedGroupCallRules, tiles, pinnedId, speech)
    val shownName = when {
        shown == null -> strings["call.participant.you"]
        shown.displayName.isNotEmpty() -> shown.displayName
        shown.isLocal -> strings["call.participant.you"]
        else -> strings["call.participant.unnamed"]
    }
    val picture: MiniWindowPicture = when {
        shown == null -> MiniWindowPicture.Avatar(shownName, shownName, null, ownPerson = true)
        shown.isScreen -> MiniWindowPicture.Share(screens[sharerParticipantId(shown.id)], shownName)
        shown.isLocal -> {
            val track = localTrack
            if (shown.camOn && track != null) {
                MiniWindowPicture.Video(track, mirrored = frontFacing)
            } else {
                MiniWindowPicture.Avatar(shownName, shown.id, null, ownPerson = true)
            }
        }
        shown.camOn -> {
            val main = mains[shown.id]
            if (main != null) {
                MiniWindowPicture.GroupCamera(shown.id, main, middles[shown.id], lights[shown.id], given[shown.id])
            } else {
                MiniWindowPicture.Waiting
            }
        }
        else -> MiniWindowPicture.Avatar(
            name = shownName,
            seed = shown.id,
            personKey = me.lakko.app.ui.components.personKeyBytes(shown.personKeyB64),
            ownPerson = false,
        )
    }
    val shownIsLocal = shown?.isLocal != false
    val people = participants.size
    return CallMiniWindowModel(
        bar = bar,
        collapsed = collapsed,
        shownName = shownName,
        peopleBeyond = miniWindowPeopleBeyond(people, shownIsLocal = shownIsLocal, groupCall = true),
        picture = picture,
        // "none when the picture already is you".
        ownInset = if (shownIsLocal) {
            null
        } else {
            localTrack?.takeIf { local?.camOn == true }?.let { MiniWindowOwnVideo(it, mirrored = frontFacing) }
        },
        screenShare = shown?.isScreen == true,
        people = people,
        groupUi = ui,
    )
}

/**
 * The status line, the lock and the mute button, decided as the bar decided
 * them. `null` when the shared rule says there is nothing to put away (a call
 * only ringing in, a call that is over).
 *
 * Whether the call's own screen is up is NOT asked here: the same answer
 * feeds both the window (shown only while collapsed) and the system
 * picture-in-picture window (shown full screen or collapsed), so the rule is
 * asked as if the screen were down and the caller adds the collapse.
 */
@Composable
@Suppress("LongParameterList")
private fun callBarModel(
    who: String,
    phase: CallBarPhase,
    connectedSinceMs: Long?,
    showsLock: Boolean,
    cameraSending: Boolean,
    micOn: Boolean,
): CallBarModel? {
    if (!callBarVisible(phase = phase, callScreenUp = false)) return null
    // The tick reads the wall clock rather than counting its own iterations,
    // so a dropped frame or a doze does not make the window's clock drift
    // away from the call screen's.
    var nowMs by remember { mutableStateOf(System.currentTimeMillis()) }
    LaunchedEffect(connectedSinceMs) {
        if (connectedSinceMs == null) return@LaunchedEffect
        while (true) {
            nowMs = System.currentTimeMillis()
            delay(1_000)
        }
    }
    val connectedForMs = connectedSinceMs?.let { (nowMs - it).coerceAtLeast(0L) } ?: 0L
    return CallBarModel(
        who = who,
        status = callBarStatus(phase = phase, connectedForMs = connectedForMs.toULong()),
        showsLock = showsLock,
        cameraSending = cameraSending,
        // Section 4.1's state table: on the `Calling...` row only End is
        // active, and the window has no End.
        mic = callBarMicControl(micOn = micOn, ringing = phase == CallBarPhase.CALLING),
        micOn = micOn,
    )
}

// ---------------------------------------------------------------------------
// The floating window
// ---------------------------------------------------------------------------

/**
 * The window over the app, draggable, resting in a corner, tucked past a side
 * edge on a phone. Draws nothing unless [model] is collapsed.
 *
 * Fills its parent: it measures the screen to place itself and draws only
 * the window (and the tucked strip); the rest of the area stays empty and
 * takes no touches.
 */
@Composable
internal fun CallMiniWindowHost(
    model: CallMiniWindowModel?,
    onExpand: () -> Unit,
    onToggleMute: (on: Boolean) -> Unit,
) {
    if (model == null || !model.collapsed) return
    // Section 4.5.1: the stage follows whoever is speaking, which is read
    // only while something shows the stage.
    val ui = model.groupUi
    DisposableEffect(ui) {
        ui?.watchSpeaking(true)
        onDispose { ui?.watchSpeaking(false) }
    }
    BoxWithConstraints(modifier = Modifier.fillMaxSize()) {
        val density = LocalDensity.current
        val layoutDirection = LocalLayoutDirection.current
        val rtl = layoutDirection == LayoutDirection.Rtl
        val screenW = maxWidth.value
        val screenH = maxHeight.value
        val sizeDp = miniWindowSize(screenW, screenH, model.screenShare)
        val tuckAllowed = miniWindowTuckAllowed(screenW, screenH)
        val px = density.density
        // The area the window's top-left ranges over, in px: inside the safe
        // edges (system bars, cutout, software keyboard), below the screen's
        // own header and above a tab bar or composer, 16 dp in. The keyboard is part of the safe-drawing
        // insets, so a window in a bottom corner rises with it.
        val safe = WindowInsets.safeDrawing
        val margin = MINI_WINDOW_MARGIN_DP * px
        val area = MiniWindowArea(
            left = safe.getLeft(density, layoutDirection) + margin,
            top = safe.getTop(density) + MINI_WINDOW_HEADER_DP * px + margin,
            right = constraints.maxWidth - safe.getRight(density, layoutDirection) - margin,
            bottom = constraints.maxHeight - safe.getBottom(density) - MINI_WINDOW_FOOTER_DP * px - margin,
        )
        val sizePx = MiniWindowSize(sizeDp.width * px, sizeDp.height * px)
        // Corners are named start/end; the geometry is in screen x, so a
        // right-to-left layout mirrors the start side.
        fun screenCorner(c: MiniWindowCorner): MiniWindowCorner =
            if (rtl) MiniWindowCorner.of(top = c.top, start = !c.start) else c

        var corner by remember { mutableStateOf(MiniWindowPlacement.corner) }
        var tucked by remember { mutableStateOf<Boolean?>(null) } // true = left edge, false = right edge
        val position = remember { Animatable(Offset.Zero, Offset.VectorConverter) }
        var placed by remember { mutableStateOf(false) }
        var dragging by remember { mutableStateOf(false) }
        // Where a finger holds the window right now; `null` when nobody does.
        // Drawn from directly, so a drag follows the finger frame by frame
        // without racing the corner animation.
        var held by remember { mutableStateOf<Offset?>(null) }
        val scope = rememberCoroutineScope()

        val target = miniWindowCornerPosition(screenCorner(corner), area, sizePx)
        LaunchedEffect(target, dragging) {
            if (dragging) return@LaunchedEffect
            val to = Offset(target.x, target.y)
            if (!placed) {
                position.snapTo(to)
                placed = true
            } else {
                position.animateTo(to, spring(dampingRatio = 0.85f, stiffness = 400f))
            }
        }

        val stripLeft = tucked
        if (stripLeft != null) {
            TuckedStrip(
                atLeft = stripLeft,
                label = model.bar.who,
                centreY = target.y + sizePx.height / 2f,
                maxWidthPx = constraints.maxWidth.toFloat(),
                safeLeftPx = safe.getLeft(density, layoutDirection).toFloat(),
                safeRightPx = safe.getRight(density, layoutDirection).toFloat(),
                onShow = { tucked = null },
            )
            return@BoxWithConstraints
        }

        val windowWidthPx = miniWindowWidthPx(sizeDp.width, px)
        Box(
            modifier = Modifier
                .absoluteOffset {
                    val at = held ?: position.value
                    IntOffset(at.x.toInt(), at.y.toInt())
                }
                .size(width = sizeDp.width.dp, height = sizeDp.height.dp)
                .pointerInput(area, sizePx, tuckAllowed, rtl) {
                    val tracker = VelocityTracker()
                    var dragPos = Offset.Zero
                    detectDragGestures(
                        onDragStart = {
                            dragging = true
                            tracker.resetTracking()
                            dragPos = position.value
                            held = dragPos
                        },
                        onDragEnd = {
                            val v = tracker.calculateVelocity()
                            val rest = miniWindowRelease(
                                position = MiniWindowPoint(dragPos.x, dragPos.y),
                                velocity = MiniWindowPoint(v.x, v.y),
                                area = area,
                                size = sizePx,
                                tuckAllowed = tuckAllowed,
                            )
                            when (rest) {
                                is MiniWindowRest.InCorner -> {
                                    val landed = screenCorner(rest.corner)
                                    corner = landed
                                    MiniWindowPlacement.corner = landed
                                }
                                // Comes back to the corner it came from.
                                is MiniWindowRest.Tucked -> tucked = rest.atStart
                            }
                            // The corner animation starts from where the
                            // finger let go.
                            scope.launch {
                                position.snapTo(dragPos)
                                held = null
                                dragging = false
                            }
                        },
                        onDragCancel = {
                            scope.launch {
                                position.snapTo(dragPos)
                                held = null
                                dragging = false
                            }
                        },
                    ) { change, amount ->
                        change.consume()
                        dragPos += amount
                        tracker.addPosition(change.uptimeMillis, dragPos)
                        held = dragPos
                    }
                },
        ) {
            CallMiniWindowFace(
                model = model.bar,
                shownName = model.shownName,
                peopleBeyond = model.peopleBeyond,
                insetAtStart = miniWindowInsetAtStart(corner),
                onExpand = onExpand,
                onToggleMute = onToggleMute,
                picture = { MiniWindowPictureView(model, windowWidthPx) },
                ownInset = model.ownInset?.let { own -> { MiniWindowOwnInset(own) } },
            )
        }
    }
}

/**
 * What a window tucked past a side edge leaves behind: a strip with a
 * chevron pointing back in (section 4.5.2). One button, `Show call`; tapping
 * it or dragging it back in returns the window to the corner it came from.
 */
@Composable
private fun TuckedStrip(
    atLeft: Boolean,
    label: String,
    centreY: Float,
    maxWidthPx: Float,
    safeLeftPx: Float,
    safeRightPx: Float,
    onShow: () -> Unit,
) {
    val density = LocalDensity.current
    val stripW = MINI_WINDOW_TUCK_STRIP_WIDTH_DP * density.density
    val stripH = MINI_WINDOW_TUCK_STRIP_HEIGHT_DP * density.density
    val x = if (atLeft) safeLeftPx else maxWidthPx - safeRightPx - stripW
    val y = centreY - stripH / 2f
    val strings = LocalStrings.current
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    // The catalog has no `Show call` yet (it is a shared catalog this client
    // does not own); the spoken name is the window's own return label.
    val spoken = strings.format("call.bar.return_a11y", mapOf("name" to label))
    val shape = if (atLeft) {
        RoundedCornerShape(topEnd = 12.dp, bottomEnd = 12.dp)
    } else {
        RoundedCornerShape(topStart = 12.dp, bottomStart = 12.dp)
    }
    var drag by remember { mutableStateOf(0f) }
    Box(
        modifier = Modifier
            .absoluteOffset { IntOffset(x.toInt(), y.toInt()) }
            .size(MINI_WINDOW_TUCK_STRIP_WIDTH_DP.dp, MINI_WINDOW_TUCK_STRIP_HEIGHT_DP.dp)
            .shadow(8.dp, shape)
            .clip(shape)
            .background(LakkoDark.surface)
            .pointerInput(atLeft) {
                detectHorizontalDragGestures(
                    onDragStart = { drag = 0f },
                    onDragEnd = {
                        // A pull back in of half the strip's width.
                        if (miniWindowStripPulledOut(atLeft, drag, stripW)) onShow()
                        drag = 0f
                    },
                ) { change, amount ->
                    change.consume()
                    drag += amount
                }
            }
            .clickable(onClick = onShow)
            .semantics(mergeDescendants = true) {
                contentDescription = spoken
                role = Role.Button
            },
        contentAlignment = Alignment.Center,
    ) {
        LkIcon(
            // Points back into the screen. The arrows mirror themselves in a
            // right-to-left layout, and the strip's edge is a physical one.
            icon = if (atLeft != rtl) {
                R.drawable.ic_lk_chevron_forward
            } else {
                R.drawable.ic_lk_chevron_back
            },
            contentDescription = null,
            tint = LakkoDark.text,
            size = me.lakko.app.ui.components.LkIconSize.Row,
        )
    }
}

// ---------------------------------------------------------------------------
// The face: what the window and the system picture-in-picture window draw
// ---------------------------------------------------------------------------

/** Section 4.5.1: 36 dp circle in a 48 dp target, as the bar drew it. */
private val MUTE_TARGET = 48.dp
private val MUTE_CIRCLE = 36.dp

/** Section 4.1.1: "Disabled is 40 percent opacity, never a hidden button." */
private const val DISABLED_ALPHA = 0.4f

/**
 * The face of the window, in section 4.5.1's order: the picture, the state or
 * the running time top left with the lock beside it when the call screen
 * would show its chip, the one round mute button top right, the shown
 * person's name and `+N` at the bottom, and the own camera inset in the
 * bottom corner away from the edge the window sits on.
 *
 * Two touch targets: everything but the mute button brings the call back.
 *
 * @param compact the system picture-in-picture window (section 4.5.4): fills
 *   the system's window, has no corners or shadow of its own, and no mute
 *   button and no tap — the system draws its own controls and the mute is its
 *   one action.
 */
@Composable
@Suppress("LongParameterList")
internal fun CallMiniWindowFace(
    model: CallBarModel,
    shownName: String,
    peopleBeyond: Int,
    insetAtStart: Boolean,
    onExpand: () -> Unit,
    onToggleMute: (on: Boolean) -> Unit,
    picture: @Composable BoxScope.() -> Unit,
    ownInset: (@Composable BoxScope.() -> Unit)?,
    modifier: Modifier = Modifier,
    compact: Boolean = false,
) {
    val strings = LocalStrings.current
    val status = callBarStatusText(model.status, strings)
    // What TalkBack reads for the window (section 4.5.6 "Spoken"): the
    // return and who the call is with, the state or how long it has been
    // running in words, then the lock and the camera when they apply.
    val spokenState = when (val st = model.status) {
        CallBarStatus.Calling -> strings["call.status.calling_spoken"]
        CallBarStatus.Connecting -> strings["call.status.connecting_spoken"]
        CallBarStatus.Reconnecting -> strings["call.status.reconnecting_spoken"]
        is CallBarStatus.Elapsed -> spokenCallDuration((st.ms / 1_000uL).toLong(), strings) ?: status
    }
    val spoken = joinList(
        buildList {
            add(strings.format("call.bar.return_a11y", mapOf("name" to model.who)))
            add(spokenState)
            if (model.showsLock) add(strings["call.bar.encrypted_a11y"])
            if (model.cameraSending) add(strings["call.bar.camera_on_a11y"])
        },
        strings,
    )
    // The live region carries the state only and not the running time, so
    // `Reconnecting...` is heard without a clock being read out every second.
    val liveState = when (model.status) {
        CallBarStatus.Calling -> strings["call.status.calling_spoken"]
        CallBarStatus.Connecting -> strings["call.status.connecting_spoken"]
        CallBarStatus.Reconnecting -> strings["call.status.reconnecting_spoken"]
        is CallBarStatus.Elapsed -> callStatusText(
            calling = false,
            connecting = false,
            connected = true,
            reconnecting = false,
            ended = false,
            elapsedSecs = null,
            strings = strings,
        )
    }
    val shape = RoundedCornerShape(12.dp)
    val frame = if (compact) {
        modifier.fillMaxSize()
    } else {
        modifier
            .fillMaxSize()
            // --shadow-2, the dialog shadow: it floats over content the same way.
            .shadow(8.dp, shape)
            .clip(shape)
    }
    Box(modifier = frame.background(LakkoDark.background)) {
        // The return target, under everything else so the mute button keeps
        // its own press.
        Box(
            modifier = Modifier
                .fillMaxSize()
                .then(
                    if (compact) {
                        Modifier.clearAndSetSemantics { contentDescription = spoken }
                    } else {
                        Modifier
                            .clickable(onClick = onExpand)
                            .semantics(mergeDescendants = true) {
                                contentDescription = spoken
                                role = Role.Button
                            }
                    },
                ),
        ) {
            Box(modifier = Modifier.fillMaxSize().clearAndSetSemantics { }) { picture() }
        }
        // Scrims so the white text reads over any picture.
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(40.dp)
                .align(Alignment.TopCenter)
                .background(Brush.verticalGradient(listOf(Color.Black.copy(alpha = 0.55f), Color.Transparent))),
        )
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(36.dp)
                .align(Alignment.BottomCenter)
                .background(Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.6f)))),
        )
        // Top left: the state or the time, and the lock.
        Row(
            modifier = Modifier
                .align(Alignment.TopStart)
                .padding(start = 8.dp, top = 8.dp, end = if (compact) 8.dp else MUTE_TARGET)
                .clearAndSetSemantics { },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                text = status,
                color = LakkoDark.text,
                fontSize = 12.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
            )
            if (model.showsLock) {
                LkIcon(
                    icon = R.drawable.ic_lk_lock_fill,
                    contentDescription = null,
                    tint = LakkoDark.success,
                    size = LkIconSize.Mark,
                )
            }
        }
        // The live region, without a visible face of its own.
        Box(
            modifier = Modifier
                .size(1.dp)
                .semantics {
                    contentDescription = liveState
                    liveRegion = LiveRegionMode.Polite
                },
        )
        if (!compact) {
            MiniWindowMuteButton(
                model = model,
                onToggleMute = onToggleMute,
                modifier = Modifier.align(Alignment.TopEnd),
            )
        }
        // The own camera inset, in the bottom corner away from the edge, and
        // the shown person's name and `+N` beside it at the bottom.
        BoxWithConstraints(modifier = Modifier.fillMaxSize()) {
            val insetW = maxWidth * 0.3f
            val insetSide = if (ownInset != null) insetW + 12.dp else 8.dp
            if (ownInset != null) {
                Box(
                    modifier = Modifier
                        .align(if (insetAtStart) Alignment.BottomStart else Alignment.BottomEnd)
                        .padding(6.dp)
                        .size(width = insetW, height = insetW * 4f / 3f)
                        .clip(RoundedCornerShape(6.dp))
                        .background(LakkoDark.surface)
                        .clearAndSetSemantics { },
                ) { ownInset() }
            }
            Row(
                modifier = Modifier
                    .align(Alignment.BottomStart)
                    .fillMaxWidth()
                    .padding(
                        start = if (insetAtStart) insetSide else 8.dp,
                        end = if (insetAtStart) 8.dp else insetSide,
                        bottom = 6.dp,
                    )
                    .clearAndSetSemantics { },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = shownName,
                    color = LakkoDark.text,
                    fontSize = 12.sp,
                    fontWeight = FontWeight.Medium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                if (peopleBeyond > 0) {
                    Spacer(Modifier.width(4.dp))
                    Text(
                        text = "+$peopleBeyond",
                        color = LakkoDark.textMuted,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Medium,
                        maxLines = 1,
                    )
                }
            }
        }
    }
}

/**
 * The one control, drawn on and off exactly as the call screen draws it: the
 * window sits on the call surface, so the call screen's own palette applies.
 */
@Composable
private fun MiniWindowMuteButton(
    model: CallBarModel,
    onToggleMute: (on: Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    val control = model.mic
    val spoken = control.spokenLabel.text()
    Box(
        modifier = modifier
            .size(MUTE_TARGET)
            .alpha(if (control.enabled) 1f else DISABLED_ALPHA)
            .clickable(enabled = control.enabled, onClick = { onToggleMute(!model.micOn) })
            .semantics(mergeDescendants = true) {
                contentDescription = spoken
                role = Role.Button
                if (!control.enabled) disabled()
            },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(MUTE_CIRCLE)
                .clip(CircleShape)
                // The outline state sits on a picture, so it keeps the dark
                // scrim under its 12 percent white rather than vanishing.
                .background(Color.Black.copy(alpha = 0.35f))
                .background(control.emphasis.fill()),
            contentAlignment = Alignment.Center,
        ) {
            LkIcon(
                icon = control.icon(),
                contentDescription = null,
                tint = control.emphasis.foreground(),
                modifier = Modifier.clearAndSetSemantics { },
                size = LkIconSize.Row,
            )
        }
    }
}

/**
 * The shown person's picture. A group camera asks the SFU for the picture
 * the window's width warrants (section 4.5.1) and nothing else: every tile of
 * the call screen is gone while the call is collapsed, so this is the only
 * thing that asks.
 */
@Composable
internal fun BoxScope.MiniWindowPictureView(model: CallMiniWindowModel, windowWidthPx: Int) {
    val egl = remember { WebRtcPeerConnection.sharedEglContext() }
    when (val picture = model.picture) {
        is MiniWindowPicture.GroupCamera -> {
            val ui = model.groupUi
            val hasLight = picture.light != null
            val wanted = remember(model.people, windowWidthPx, hasLight) {
                askedQuality(tileQualityOf(SharedGroupCallRules, model.people, windowWidthPx, false), hasLight)
            }
            val key = remember { Any() }
            if (ui != null) {
                LaunchedEffect(key, picture.participantId, wanted) {
                    ui.reportTileVideoQuality(key, picture.participantId, wanted)
                }
                DisposableEffect(key, picture.participantId) {
                    onDispose { ui.reportTileVideoQuality(key, picture.participantId, null) }
                }
            }
            if (egl != null) {
                val heavy = heavyTrackForGiven(
                    wanted = wanted,
                    given = picture.given,
                    main = picture.main,
                    middle = picture.middle,
                    hasLight = hasLight,
                )
                RemoteLayeredVideoViewForWindow(picture, heavy, egl)
            }
        }
        is MiniWindowPicture.Video -> if (egl != null) {
            me.lakko.app.calls.LocalVideoPreviewView(
                track = picture.track,
                eglContext = egl,
                modifier = Modifier.fillMaxSize(),
                mirrored = picture.mirrored,
                scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
            )
        }
        is MiniWindowPicture.Share -> ScreenSharePicture(
            track = picture.track,
            sharerName = picture.sharerName,
            zoomable = false,
            compact = true,
            modifier = Modifier.fillMaxSize(),
        )
        MiniWindowPicture.Waiting -> Box(
            modifier = Modifier.fillMaxSize().background(LakkoDark.sunken),
            contentAlignment = Alignment.Center,
        ) {
            CircularProgressIndicator(
                modifier = Modifier.size(20.dp),
                color = LakkoDark.textMuted,
                strokeWidth = 2.dp,
            )
        }
        is MiniWindowPicture.Avatar -> MiniWindowAvatar(picture)
    }
}

@Composable
private fun BoxScope.RemoteLayeredVideoViewForWindow(
    picture: MiniWindowPicture.GroupCamera,
    heavy: VideoTrack?,
    egl: org.webrtc.EglBase.Context,
) {
    me.lakko.app.calls.RemoteLayeredVideoView(
        heavyTrack = heavy,
        heavyRole = if (picture.middle != null && heavy === picture.middle) AvRole.MIDDLE else AvRole.MAIN,
        lightTrack = picture.light,
        peerId = peerIdOfParticipant(picture.participantId),
        eglContext = egl,
        modifier = Modifier.fillMaxSize(),
        scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
    )
}

/** The avatar on the call surface, as the person's tile draws it: their
 *  photo when this device holds it, else their initials on their colour. */
@Composable
private fun BoxScope.MiniWindowAvatar(picture: MiniWindowPicture.Avatar) {
    BoxWithConstraints(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        val diameter: Dp = minOf(maxWidth, maxHeight) * 0.5f
        if (picture.ownPerson) {
            val tint = remember(picture.seed) { avatarTintFor(picture.seed, dark = true) }
            val own = me.lakko.app.ui.components.rememberOwnAvatar()
            val still = me.lakko.app.ui.components.rememberPersonStill(null, avatar = own)
            Box(
                modifier = Modifier.size(diameter).clip(CircleShape).background(tint.background),
                contentAlignment = Alignment.Center,
            ) {
                if (still != null) {
                    val photo = remember(still) { still.asImageBitmap() }
                    androidx.compose.foundation.Image(
                        bitmap = photo,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier.fillMaxSize(),
                    )
                } else {
                    Text(
                        text = initialsOf(picture.name),
                        color = tint.foreground,
                        fontSize = (diameter.value * 0.38f).sp,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                    )
                }
            }
        } else {
            LakkoAvatar(
                name = picture.name,
                seed = picture.seed,
                size = diameter,
                personKey = picture.personKey,
            )
        }
    }
}

/** This device's own camera, mirrored as on the call screen, drawn above the
 *  picture's own video surface. */
@Composable
private fun BoxScope.MiniWindowOwnInset(own: MiniWindowOwnVideo) {
    val egl = remember { WebRtcPeerConnection.sharedEglContext() } ?: return
    me.lakko.app.calls.LocalVideoPreviewView(
        track = own.track,
        eglContext = egl,
        modifier = Modifier.fillMaxSize(),
        mirrored = own.mirrored,
        scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
        mediaOverlay = true,
    )
}

// ---------------------------------------------------------------------------
// The system picture-in-picture window
// ---------------------------------------------------------------------------

/**
 * What the activity draws while it is in the system's picture-in-picture
 * window during a call (section 4.5.4): exactly what the floating window
 * shows, filling the system's window, with no controls of its own (the mute
 * is the system window's one action).
 */
@Composable
internal fun CallPictureInPictureContent(model: CallMiniWindowModel) {
    val ui = model.groupUi
    DisposableEffect(ui) {
        ui?.watchSpeaking(true)
        onDispose { ui?.watchSpeaking(false) }
    }
    BoxWithConstraints(modifier = Modifier.fillMaxSize().background(LakkoDark.background)) {
        val widthPx = miniWindowWidthPx(maxWidth.value, LocalDensity.current.density)
        CallMiniWindowFace(
            model = model.bar,
            shownName = model.shownName,
            peopleBeyond = model.peopleBeyond,
            insetAtStart = false,
            onExpand = {},
            onToggleMute = {},
            picture = { MiniWindowPictureView(model, widthPx) },
            ownInset = model.ownInset?.let { own -> { MiniWindowOwnInset(own) } },
            compact = true,
        )
    }
}

/**
 * Keep the activity's picture-in-picture parameters in step with the call
 * (section 4.5.4): auto-enter while a call can be put away, the window's
 * shape, and the one microphone action. Turned off when the call goes, and
 * when this leaves the composition (sign-out).
 *
 * A call that ends while the system window is up takes the window with it:
 * the task goes to the back rather than leaving the app's screens playing in
 * a small window nobody asked for.
 */
@Composable
internal fun CallPictureInPictureEffect(model: CallMiniWindowModel?) {
    val activity = androidx.compose.ui.platform.LocalContext.current.findCallActivity() ?: return
    val spec = me.lakko.app.calls.pictureInPictureSpec(
        live = model != null,
        screenShare = model?.screenShare == true,
        micOn = model?.bar?.micOn != false,
        micEnabled = model?.bar?.mic?.enabled == true,
    )
    androidx.compose.runtime.SideEffect { me.lakko.app.calls.CallPictureInPicture.update(activity, spec) }
    DisposableEffect(activity) {
        onDispose { me.lakko.app.calls.CallPictureInPicture.update(activity, null) }
    }
    val inPip by me.lakko.app.calls.CallPictureInPicture.active.collectAsStateWithLifecycle()
    LaunchedEffect(inPip, spec == null) {
        if (inPip && spec == null) activity.moveTaskToBack(false)
    }
}

private tailrec fun android.content.Context.findCallActivity(): android.app.Activity? = when (this) {
    is android.app.Activity -> this
    is android.content.ContextWrapper -> baseContext.findCallActivity()
    else -> null
}

/**
 * Keep the call's foreground service and its ongoing notification in step
 * with the call (section 4.5.4): started when a call is placed or answered,
 * its text and camera type kept current while the app is on the screen, and
 * stopped when the call is over. The service also stops itself when the call
 * ends while the app is away, so nothing here has to run in the background.
 *
 * Re-tried whenever the app comes back to the screen, because a start the
 * platform refused (the app was not in the foreground) is not remembered as
 * done.
 */
@Composable
internal fun OngoingCallServiceEffect(model: CallMiniWindowModel?) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val lifecycle = androidx.lifecycle.compose.LocalLifecycleOwner.current.lifecycle
    val lifecycleState by lifecycle.currentStateFlow.collectAsState()
    val onScreen = lifecycleState.isAtLeast(androidx.lifecycle.Lifecycle.State.STARTED)
    val spec = model?.let { me.lakko.app.calls.OngoingCallSpec(label = it.bar.who, cameraSending = it.bar.cameraSending) }
    LaunchedEffect(spec, onScreen) {
        if (onScreen) me.lakko.app.calls.OngoingCallNotice.update(context, spec)
    }
}
