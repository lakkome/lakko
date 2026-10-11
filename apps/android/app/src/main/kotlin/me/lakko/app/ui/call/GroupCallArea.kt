// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.GridView
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material.icons.filled.ViewDay
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.PointerEventType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlin.math.abs
import me.lakko.app.calls.CallTimers
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.GridLayout
import me.lakko.app.calls.GroupCallRules
import me.lakko.app.calls.GroupLayoutKind
import me.lakko.app.calls.LocalVideoPreviewView
import me.lakko.app.calls.RemoteLayeredVideoView
import me.lakko.app.calls.SettledWidth
import me.lakko.app.calls.TileControlsReveal
import me.lakko.app.calls.WebRtcPeerConnection
import me.lakko.app.calls.askedQuality
import me.lakko.app.calls.connectionIsWorthShowing
import me.lakko.app.calls.gridBoxSize
import me.lakko.app.calls.gridLayout
import me.lakko.app.calls.gridPage
import me.lakko.app.calls.groupLayoutKind
import me.lakko.app.calls.heavyTrackForGiven
import me.lakko.app.calls.peerIdOfParticipant
import me.lakko.app.calls.physicalTileWidth
import me.lakko.app.calls.stagePerson
import me.lakko.app.calls.stageSize
import me.lakko.app.calls.stripMetrics
import me.lakko.app.calls.stripPeople
import me.lakko.app.calls.tilePlace
import me.lakko.app.calls.tileQualityOf
import me.lakko.app.calls.togglePin
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.joinList
import me.lakko.app.l10n.t
import me.lakko.app.ui.chat.initialsOf
import me.lakko.app.ui.theme.LakkoDark
import me.lakko.app.ui.theme.avatarTintFor
import org.webrtc.RendererCommon
import uniffi.lakko_ffi_uniffi.TileVideoQuality

/**
 * The tiles of a group call (`docs/ux/group-call-grid.md` sections 3 to 6).
 *
 * Two layouts and no scrolling. **Grid**: everyone the same size, filling the
 * space between the status line and the control bar; more people than a page
 * holds means pages. **Focus**: the person speaking, or the pinned one, large
 * in the middle, and everyone else in a row above.
 *
 * The geometry and the choice of the stage are the shared rules
 * (`calls/GroupCallLayout.kt`, over [GroupCallRules]); this composable
 * measures its own area and draws the answer. Tiles are keyed by participant
 * id: the state replaces a person's object whenever anything about them
 * changes, and a tile rebuilt on every change of the speaking ring would
 * restart its video each time.
 *
 * What it replaced was a scroll area of fixed shapes, 16:9 for one large tile
 * and 4:3 in two fixed columns for the rest, which on a phone wasted most of
 * the screen for three people and hid the last row under the control bar.
 *
 * @param modifier sizes the call AREA: the caller places this between the
 *   status line and the control bar, inside the side margins.
 */
@Composable
internal fun GroupCallArea(
    state: CallUiState,
    participants: List<CallUiState.Participant>,
    rules: GroupCallRules,
    modifier: Modifier = Modifier,
) {
    val pinnedId by state.pinnedId.collectAsState()
    val choice by state.layoutChoice.collectAsState()
    val speech by state.speech.collectAsState()
    var page by remember { mutableIntStateOf(0) }
    val kind = groupLayoutKind(choice, pinnedId, participants)

    // Section 6.2: a tile is never itself the pin control. A tap on someone
    // else's tile only brings that tile's controls up for a few seconds; the
    // pin button among them is what pins and unpins.
    val timers = remember { MainThreadTimers() }
    var revealedId by remember { mutableStateOf<String?>(null) }
    val reveal = remember(timers) { TileControlsReveal(timers) { revealedId = it } }
    DisposableEffect(reveal) { onDispose { reveal.dispose() } }
    /** The tile the press now under way began on, or `null` for no tile. */
    val pressedTile = remember { PressedTile() }
    val haptics = LocalHapticFeedback.current
    val tiles = TileEnvironment(
        rules = rules,
        people = participants.size,
        timers = timers,
        revealedId = revealedId,
        onTap = { id -> reveal.tap(id) },
        onPressBegan = { id -> pressedTile.id = id },
        onPin = { id ->
            togglePin(
                pinnedId = state.pinnedId.value,
                id = id,
                setLayoutChoice = { state.setLayoutChoice(it) },
                pin = { state.pin(it) },
            )
            reveal.hide()
            // Section 13: a pin answers with a light haptic.
            haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
        },
    )

    BoxWithConstraints(
        // A press anywhere in the call area but on the tile whose controls
        // are up hides them. This sees the press first (the initial pass runs
        // from here down to the tiles, each of which names itself), and
        // decides in the main pass, which comes back up after them.
        modifier = modifier.pointerInput(reveal) {
            awaitPointerEventScope {
                while (true) {
                    val event = awaitPointerEvent(PointerEventPass.Initial)
                    if (event.type != PointerEventType.Press) continue
                    pressedTile.id = null
                    awaitPointerEvent(PointerEventPass.Main)
                    reveal.pressBegan(pressedTile.id)
                }
            }
        },
    ) {
        // Whole dp, like the rule's own arithmetic. The strip under the tiles
        // is always there, so the grid does not change size when a second page
        // appears; it holds the page dots when there are pages.
        val width = maxWidth.value.toInt()
        val height = (maxHeight - PAGE_STRIP_HEIGHT).value.toInt().coerceAtLeast(0)
        Column(modifier = Modifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally) {
            when (kind) {
                GroupLayoutKind.GRID -> {
                    val grid = remember(rules, participants.size, width, height) {
                        gridLayout(rules, participants.size, width, height)
                    }
                    val shown = gridPage(participants, grid.perPage, page)
                    // A page that emptied because someone left falls back to
                    // the last one that still has people.
                    LaunchedEffect(shown.page) { if (page != shown.page) page = shown.page }
                    GridTiles(
                        grid = grid,
                        tiles = shown.tiles,
                        state = state,
                        pinnedId = pinnedId,
                        env = tiles,
                        page = shown.page,
                        pages = shown.pages,
                        onPage = { page = it },
                        modifier = Modifier
                            .weight(1f)
                            .fillMaxWidth(),
                    )
                    PageDots(page = shown.page, pages = shown.pages, onPage = { page = it })
                }
                GroupLayoutKind.FOCUS -> {
                    val stage = stagePerson(rules, participants, pinnedId, speech)
                    val strip = stripPeople(participants, stage)
                    val metrics = stripMetrics(width)
                    val size = remember(rules, width, height) { stageSize(rules, width, height) }
                    Column(
                        modifier = Modifier
                            .weight(1f)
                            .fillMaxWidth(),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.Center,
                    ) {
                        // The strip is the one place scrolling is allowed: its
                        // tiles are secondary and the stage never moves with it.
                        Row(
                            modifier = Modifier
                                .height(metrics.tileHeight.dp)
                                .horizontalScroll(rememberScrollState()),
                            horizontalArrangement = Arrangement.spacedBy(metrics.gap.dp),
                        ) {
                            for (person in strip) {
                                key(person.id) {
                                    GroupTile(
                                        participant = person,
                                        state = state,
                                        widthDp = metrics.tileWidth,
                                        heightDp = metrics.tileHeight,
                                        pinned = person.id == pinnedId,
                                        stage = false,
                                        env = tiles,
                                    )
                                }
                            }
                        }
                        Spacer(Modifier.height(metrics.gap.dp))
                        if (stage != null) {
                            key(stage.id) {
                                GroupTile(
                                    participant = stage,
                                    state = state,
                                    widthDp = size.width,
                                    heightDp = size.height,
                                    pinned = stage.id == pinnedId,
                                    stage = true,
                                    env = tiles,
                                )
                            }
                        }
                    }
                    Spacer(Modifier.height(PAGE_STRIP_HEIGHT))
                }
            }
        }
    }
}

/** One page of the grid: equal tiles at the places the shared rule gives. */
@Composable
@Suppress("LongParameterList")
private fun GridTiles(
    grid: GridLayout,
    tiles: List<CallUiState.Participant>,
    state: CallUiState,
    pinnedId: String?,
    env: TileEnvironment,
    page: Int,
    pages: Int,
    onPage: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    val box = gridBoxSize(grid, tiles.size)
    Box(
        modifier = modifier
            // A horizontal swipe turns the page (section 4.5). Only past the
            // touch slop and a real distance, so a tap on a tile still brings
            // its controls up: a swipe is not a tap.
            .pointerInput(page, pages, rtl) {
                if (pages <= 1) return@pointerInput
                var dragged = 0f
                detectHorizontalDragGestures(
                    onDragStart = { dragged = 0f },
                    onHorizontalDrag = { change, amount ->
                        change.consume()
                        dragged += amount
                    },
                    onDragEnd = {
                        if (abs(dragged) >= PAGE_SWIPE_DISTANCE.toPx()) {
                            val forward = if (rtl) dragged > 0f else dragged < 0f
                            onPage((page + if (forward) 1 else -1).coerceIn(0, pages - 1))
                        }
                    },
                )
            },
        contentAlignment = Alignment.Center,
    ) {
        Box(modifier = Modifier.size(width = box.width.dp, height = box.height.dp)) {
            tiles.forEachIndexed { index, person ->
                // Keyed by the person: a tile that only changes its place, or
                // its speaking ring, is the same tile with the same video.
                key(person.id) {
                    val place = tilePlace(grid, tiles.size, index)
                    GroupTile(
                        participant = person,
                        state = state,
                        widthDp = grid.tileWidth,
                        heightDp = grid.tileHeight,
                        pinned = person.id == pinnedId,
                        stage = false,
                        env = env,
                        modifier = Modifier.offset(x = place.x.dp, y = place.y.dp),
                    )
                }
            }
        }
    }
}

/**
 * The pages of the grid: one dot per page, the current one filled.
 *
 * One hit area the height of the strip, split left and right of its middle:
 * a tap on the left goes back a page, on the right forward (section 12). The
 * same two moves are accessibility actions, so the swipe is not the only way.
 */
@Composable
private fun PageDots(page: Int, pages: Int, onPage: (Int) -> Unit) {
    if (pages <= 1) {
        Spacer(Modifier.height(PAGE_STRIP_HEIGHT))
        return
    }
    val strings = LocalStrings.current
    val label = strings.format(
        "call.grid.page_a11y",
        mapOf("page" to (page + 1).toString(), "total" to pages.toString()),
    )
    val nextLabel = strings["call.grid.next_a11y"]
    val previousLabel = strings["call.grid.prev_a11y"]
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    val go: (Int) -> Unit = { delta -> onPage((page + delta).coerceIn(0, pages - 1)) }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .height(PAGE_STRIP_HEIGHT)
            .pointerInput(page, pages, rtl) {
                detectTapGestures { at ->
                    val startHalf = at.x < size.width / 2f
                    val back = if (rtl) !startHalf else startHalf
                    go(if (back) -1 else 1)
                }
            }
            .clearAndSetSemantics {
                contentDescription = label
                customActions = buildList {
                    if (page > 0) add(CustomAccessibilityAction(previousLabel) { go(-1); true })
                    if (page < pages - 1) add(CustomAccessibilityAction(nextLabel) { go(1); true })
                }
            },
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        repeat(pages) { index ->
            Box(
                modifier = Modifier
                    .size(8.dp)
                    .background(
                        if (index == page) LakkoDark.text else LakkoDark.text.copy(alpha = 0.24f),
                        CircleShape,
                    ),
            )
        }
    }
}

/**
 * Switch between the two group layouts, top right: the twin of the collapse
 * chevron at the top left (`docs/ux/group-call-grid.md` section 3).
 *
 * It shows the layout a press leads to, not the one on screen. Pinning
 * someone already means Focus, so while a pin is up the button unpins and
 * returns to the grid rather than changing a choice the pin overrides. The
 * caller does not draw it while the person is alone in the call.
 */
@Composable
internal fun GroupLayoutSwitch(
    state: CallUiState,
    participants: List<CallUiState.Participant>,
    modifier: Modifier = Modifier,
) {
    val pinnedId by state.pinnedId.collectAsState()
    val choice by state.layoutChoice.collectAsState()
    val kind = groupLayoutKind(choice, pinnedId, participants)
    val label = t(if (kind == GroupLayoutKind.GRID) "call.layout.focus_a11y" else "call.layout.grid_a11y")
    Box(
        modifier = modifier
            .size(44.dp)
            .clip(CircleShape)
            .background(LakkoDark.background.copy(alpha = 0.45f))
            .clickable {
                if (kind == GroupLayoutKind.FOCUS) {
                    state.pin(null)
                    state.setLayoutChoice(GroupLayoutKind.GRID)
                } else {
                    state.setLayoutChoice(GroupLayoutKind.FOCUS)
                }
            }
            .semantics(mergeDescendants = true) {
                contentDescription = label
                role = Role.Button
            },
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            imageVector = if (kind == GroupLayoutKind.GRID) Icons.Filled.ViewDay else Icons.Filled.GridView,
            contentDescription = null,
            tint = LakkoDark.text,
            modifier = Modifier.size(22.dp),
        )
    }
}

/**
 * One person's tile, exactly [widthDp] by [heightDp]
 * (`docs/ux/group-call-grid.md` section 7).
 *
 * The picture when the person sends one, cropped to fill; otherwise their
 * initials on their own colour, scaled to the tile. Along the bottom, in this
 * order and only where it applies: a mic-off glyph, the name, the connection
 * (fair or worse only), a pin. Nothing sits over the middle of the tile: the
 * mic-off disc that used to cover a muted person's face is gone, the strip
 * already says it.
 *
 * The speaking ring is a border inside the tile's edge, so the tile does not
 * change size when someone starts to speak.
 *
 * ## Pin (section 6.2)
 *
 * The tile is never the pin control. A tap on someone else's tile brings its
 * controls up for a few seconds and does nothing else; the round pin button
 * in the top corner, 44 dp to the touch, is what pins. The pinned tile, which
 * is always the stage, carries a persistent `Pinned` chip in that corner and
 * a tap on the chip unpins. This device's own tile has neither: it is never
 * on the stage while anyone else is present. Pin and unpin are also custom
 * accessibility actions of the tile, so no gesture is the only way in.
 *
 * ## Which picture (section 7.6)
 *
 * Someone else's tile asks the shared rule which of the person's pictures it
 * shows, from the people in the call, its own width in physical pixels and
 * whether it is the pinned person or the stage, tells the call
 * (`VideoWants.kt`), and draws the lower of that and what the SFU says it
 * gives (`VideoGiven.kt`), with the light copy beside it (section 7.5).
 *
 * @param stage the tile is the large one of the Focus layout.
 */
@Composable
@Suppress("LongParameterList", "LongMethod", "CyclomaticComplexMethod")
private fun GroupTile(
    participant: CallUiState.Participant,
    state: CallUiState,
    widthDp: Int,
    heightDp: Int,
    pinned: Boolean,
    stage: Boolean,
    env: TileEnvironment,
    modifier: Modifier = Modifier,
) {
    val localVideoTrack by state.localVideoTrack.collectAsState()
    val remoteVideoTracks by state.remoteVideoTracks.collectAsState()
    val remoteLightVideoTracks by state.remoteLightVideoTracks.collectAsState()
    val remoteMiddleVideoTracks by state.remoteMiddleVideoTracks.collectAsState()
    val remoteVideoGiven by state.remoteVideoGiven.collectAsState()
    val frontFacing by state.cameraFrontFacing.collectAsState()
    val videoPublishState by state.videoPublishState.collectAsState()
    val cameraStarting = participant.isLocal &&
        videoPublishState == CallUiState.VideoPublishState.Starting

    val strings = LocalStrings.current
    val name = participant.displayName.takeIf { it.isNotEmpty() }
        ?: strings[if (participant.isLocal) "call.participant.you" else "call.participant.unnamed"]
    val spoken = joinList(
        listOfNotNull(
            name,
            strings["call.participant.muted"].takeIf { !participant.micOn },
            strings["call.participant.pinned"].takeIf { pinned },
        ),
        strings,
    )
    val hasPinButton = !participant.isLocal
    val pinLabel = strings.format(
        if (pinned) "call.participant.unpin_a11y" else "call.participant.pin_a11y",
        mapOf("name" to name),
    )
    val controlsShown = hasPinButton && env.revealedId == participant.id
    val shape = RoundedCornerShape(12.dp)
    // A strip tile is short: its initials sit above the name line.
    val short = heightDp <= SHORT_TILE_HEIGHT_DP

    // ---- Which picture of the person this tile shows (section 7.6) --------
    val mainTrack = if (participant.isLocal) null else remoteVideoTracks[participant.id]
    val middleTrack = remoteMiddleVideoTracks[participant.id]
    val lightTrack = remoteLightVideoTracks[participant.id]
    // The tile's width in physical pixels, settled: the first measure at
    // once, a later one only after it has stood still, so a rotation or a
    // layout change does not ask the sender for a key frame on every step.
    val widthPx = physicalTileWidth(widthDp, LocalDensity.current.density)
    var settledWidthPx by remember { mutableStateOf<Int?>(null) }
    val settle = remember(env.timers) { SettledWidth(env.timers) { settledWidthPx = it } }
    DisposableEffect(settle) { onDispose { settle.dispose() } }
    LaunchedEffect(settle, widthPx, participant.isLocal) {
        // A tile not laid out yet has no width to ask about.
        if (!participant.isLocal && widthPx > 0) settle.observe(widthPx)
    }
    val large = stage || pinned
    val hasLight = lightTrack != null
    val wanted: TileVideoQuality? = settledWidthPx?.let { width ->
        remember(env.rules, env.people, width, large, hasLight) {
            askedQuality(tileQualityOf(env.rules, env.people, width, large), hasLight)
        }
    }
    // Tell the call what this tile shows; a tile that draws no picture of the
    // person (camera off, no track yet, not measured yet) shows none.
    val tileKey = remember { Any() }
    val drawsPicture = !participant.isLocal && participant.camOn && mainTrack != null
    LaunchedEffect(tileKey, participant.id, wanted, drawsPicture) {
        if (!participant.isLocal) {
            state.reportTileVideoQuality(tileKey, participant.id, if (drawsPicture) wanted else null)
        }
    }
    DisposableEffect(tileKey, participant.id) {
        // The tile is gone: it shows no picture of the person.
        onDispose { if (!participant.isLocal) state.reportTileVideoQuality(tileKey, participant.id, null) }
    }

    val tapSource = remember { MutableInteractionSource() }
    Box(
        modifier = modifier
            .size(width = widthDp.dp, height = heightDp.dp)
            .clip(shape)
            .background(LakkoDark.surface)
            .border(
                width = if (participant.speaking) 3.dp else 1.dp,
                color = if (participant.speaking) LakkoDark.accent else LakkoDark.border,
                shape = shape,
            )
            // Names itself to the call area for every press that begins on it
            // (its own controls included), so that press does not hide them.
            .pointerInput(participant.id) {
                awaitPointerEventScope {
                    while (true) {
                        val event = awaitPointerEvent(PointerEventPass.Initial)
                        if (event.type == PointerEventType.Press) env.onPressBegan(participant.id)
                    }
                }
            }
            // Reveals the controls and does nothing else. No ripple: nothing
            // was activated.
            .clickable(
                enabled = hasPinButton,
                interactionSource = tapSource,
                indication = null,
            ) { env.onTap(participant.id) }
            .semantics(mergeDescendants = true) {
                contentDescription = spoken
                if (hasPinButton) {
                    customActions = listOf(
                        CustomAccessibilityAction(pinLabel) {
                            env.onPin(participant.id)
                            true
                        },
                    )
                }
            },
    ) {
        val eglContext = remember { WebRtcPeerConnection.sharedEglContext() }
        if (!participant.isLocal && participant.camOn && mainTrack != null && eglContext != null) {
            // Sections 7.5 and 7.6: the picture this tile asked for, lowered
            // to what the SFU gives, and the person's light copy while that
            // one stands still or is being replaced. One renderer in the same
            // box with the same crop; never mirrored.
            RemoteLayeredVideoView(
                heavyTrack = heavyTrackForGiven(
                    wanted = wanted ?: TileVideoQuality.MAIN,
                    given = remoteVideoGiven[participant.id],
                    main = mainTrack,
                    middle = middleTrack,
                    hasLight = hasLight,
                ),
                lightTrack = lightTrack,
                peerId = peerIdOfParticipant(participant.id),
                eglContext = eglContext,
                modifier = Modifier.fillMaxSize(),
                scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
            )
        } else if (participant.isLocal && participant.camOn && localVideoTrack != null && eglContext != null) {
            localVideoTrack?.let { track ->
                LocalVideoPreviewView(
                    track = track,
                    eglContext = eglContext,
                    modifier = Modifier.fillMaxSize(),
                    // This device's own picture is mirrored, and only from a
                    // front camera; everyone else's never is.
                    mirrored = frontFacing,
                    // Section 4.3: camera video is cropped to fill, keeping the centre.
                    scalingType = RendererCommon.ScalingType.SCALE_ASPECT_FILL,
                )
            }
        } else if (participant.camOn) {
            // The person is sending a picture that has not arrived yet.
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(LakkoDark.sunken),
                contentAlignment = Alignment.Center,
            ) {
                CircularProgressIndicator(
                    modifier = Modifier.size(24.dp),
                    color = LakkoDark.textMuted,
                    strokeWidth = 2.dp,
                )
            }
        } else {
            val tint = remember(participant.id) { avatarTintFor(participant.id, dark = true) }
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(tint.background)
                    .padding(top = if (short) 10.dp else 0.dp),
                contentAlignment = if (short) Alignment.TopCenter else Alignment.Center,
            ) {
                Text(
                    text = initialsOf(name),
                    color = tint.foreground,
                    // Scaled to the tile, so a strip tile and a full-screen
                    // tile both read.
                    fontSize = if (short) 22.sp else initialsSizeSp(widthDp, heightDp).sp,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    modifier = Modifier.clearAndSetSemantics { },
                )
            }
        }

        // Section 6.2: the pin control, top end. The pinned tile always shows
        // it, as a `Pinned` chip; any other tile of someone else shows the
        // round button while its controls are up.
        if (hasPinButton && pinned) {
            PinnedChip(
                label = strings["call.participant.pinned"],
                actionLabel = pinLabel,
                onUnpin = { env.onPin(participant.id) },
                modifier = Modifier.align(Alignment.TopEnd),
            )
        } else if (controlsShown) {
            PinButton(
                actionLabel = pinLabel,
                onPin = { env.onPin(participant.id) },
                modifier = Modifier.align(Alignment.TopEnd),
            )
        }

        // ADR-0032 piece B2a — the camera is being acquired: said on this
        // device's own tile while the acquire and the renegotiation run.
        if (cameraStarting) {
            Row(
                modifier = Modifier
                    .align(Alignment.Center)
                    .background(LakkoDark.background.copy(alpha = 0.55f), RoundedCornerShape(8.dp))
                    .padding(horizontal = 12.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                CircularProgressIndicator(
                    modifier = Modifier.size(14.dp),
                    color = LakkoDark.text,
                    strokeWidth = 2.dp,
                )
                if (!short) {
                    Text(t("call.self.starting_camera"), color = LakkoDark.text, fontSize = 12.sp)
                }
            }
        }

        // The name strip. Anything in its normal state is absent: the strip
        // of a healthy, unmuted, unpinned person is only their name.
        Row(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                .background(LakkoDark.background.copy(alpha = 0.45f))
                .padding(horizontal = if (short) 6.dp else 10.dp, vertical = if (short) 4.dp else 8.dp)
                .clearAndSetSemantics { },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(if (short) 4.dp else 8.dp),
        ) {
            if (!participant.micOn) {
                Icon(
                    Icons.Filled.MicOff,
                    contentDescription = null,
                    tint = LakkoDark.danger,
                    modifier = Modifier.size(12.dp),
                )
            }
            Text(
                text = name,
                color = LakkoDark.text,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            // Section 7.2: a connection is drawn only when it is fair or worse.
            // A dot on every healthy tile says nothing, and for other people it
            // was never a measurement at all.
            if (connectionIsWorthShowing(participant.quality)) {
                Box(
                    modifier = Modifier
                        .size(8.dp)
                        .background(
                            if (participant.quality == CallUiState.QualityTier.Fair) {
                                LakkoDark.warning
                            } else {
                                LakkoDark.danger
                            },
                            CircleShape,
                        ),
                )
            }
            if (pinned) {
                Icon(
                    Icons.Filled.PushPin,
                    contentDescription = null,
                    tint = LakkoDark.accent,
                    modifier = Modifier.size(12.dp),
                )
            }
        }
    }
}

/** The initials' size for a tile: about a quarter of its shorter side, within
 *  the web client's bounds (`clamp(18px, 26cqmin, 72px)`). */
private fun initialsSizeSp(widthDp: Int, heightDp: Int): Float =
    (minOf(widthDp, heightDp) * 0.26f).coerceIn(18f, 72f)

/** At or below this a tile is a strip tile. */
private const val SHORT_TILE_HEIGHT_DP: Int = 110

/** The strip under the tiles that holds the page dots. */
private val PAGE_STRIP_HEIGHT = 24.dp

/** How far a horizontal drag must go to turn the page. */
private val PAGE_SWIPE_DISTANCE = 48.dp

/**
 * The round pin button of a tile whose controls are up (section 6.2): 44 dp
 * to the touch, the disc drawn smaller inside it, in the style of the other
 * tile overlays. Named from the catalog (`Pin Anna`); not selected, since the
 * pinned tile carries the chip instead.
 */
@Composable
private fun PinButton(actionLabel: String, onPin: () -> Unit, modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .size(PIN_TOUCH_TARGET)
            .clickable(onClick = onPin)
            .semantics {
                contentDescription = actionLabel
                role = Role.Button
                selected = false
            },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .size(32.dp)
                .clip(CircleShape)
                .background(LakkoDark.background.copy(alpha = 0.6f)),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                Icons.Outlined.PushPin,
                contentDescription = null,
                tint = LakkoDark.text,
                modifier = Modifier.size(16.dp),
            )
        }
    }
}

/**
 * The persistent `Pinned` chip of the pinned tile (section 6.2): the filled
 * pin glyph and the word, always shown, and a tap on it unpins. At least
 * 44 dp to the touch. Named from the catalog (`Unpin Anna`) and reports that
 * it is on.
 */
@Composable
private fun PinnedChip(label: String, actionLabel: String, onUnpin: () -> Unit, modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .height(PIN_TOUCH_TARGET)
            .clickable(onClick = onUnpin)
            .padding(horizontal = 6.dp)
            .clearAndSetSemantics {
                contentDescription = actionLabel
                role = Role.Button
                selected = true
            },
        contentAlignment = Alignment.Center,
    ) {
        Row(
            modifier = Modifier
                .clip(RoundedCornerShape(16.dp))
                .background(LakkoDark.background.copy(alpha = 0.6f))
                .padding(horizontal = 10.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Icon(
                Icons.Filled.PushPin,
                contentDescription = null,
                tint = LakkoDark.accent,
                modifier = Modifier.size(14.dp),
            )
            Text(text = label, color = LakkoDark.text, fontSize = 12.sp, maxLines = 1)
        }
    }
}

/** What every tile of the call area is handed beside its own person. */
private class TileEnvironment(
    /** The shared rules: which picture a tile shows. */
    val rules: GroupCallRules,
    /** Everyone in the call, this device's own person included. */
    val people: Int,
    val timers: CallTimers,
    /** The tile whose controls a tap brought up, or `null`. */
    val revealedId: String?,
    /** A tap on a tile: its controls come up. Never pins. */
    val onTap: (id: String) -> Unit,
    /** A press began on a tile. */
    val onPressBegan: (id: String) -> Unit,
    /** The tile's pin button or `Pinned` chip was pressed. */
    val onPin: (id: String) -> Unit,
)

/** The tile the press now under way began on. Written and read on the main
 *  thread within one pointer event. */
private class PressedTile(var id: String? = null)

/** [CallTimers] on the main thread, for the call area's own timers. */
private class MainThreadTimers : CallTimers {
    private val handler = android.os.Handler(android.os.Looper.getMainLooper())

    override fun nowMs(): Long = android.os.SystemClock.elapsedRealtime()

    override fun schedule(ms: Long, run: () -> Unit): Any {
        val task = Runnable { run() }
        handler.postDelayed(task, ms)
        return task
    }

    override fun cancel(handle: Any) {
        (handle as? Runnable)?.let { handler.removeCallbacks(it) }
    }
}

/** The pin control's touch target (section 12). */
private val PIN_TOUCH_TARGET = 44.dp
