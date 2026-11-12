// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.ui.platform.testTag
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.sp
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.t
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.KIND_GIF
import me.lakko.app.media.KIND_VIDEO_NOTE
import me.lakko.app.media.KIND_VOICE
import me.lakko.app.media.MediaBubble
import me.lakko.app.media.PendingMediaRow
import me.lakko.app.media.formatClock
import me.lakko.app.media.formatTotalClock
import me.lakko.app.media.rateLabel
import me.lakko.app.media.renderDecision
import me.lakko.app.media.spokenDuration
import me.lakko.app.media.voiceClockReservation
import me.lakko.app.media.widestRateLabel
import uniffi.lakko_ffi_uniffi.AttachmentRef
import java.io.File
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

internal const val COULD_NOT_PLAY = "chat.media.play_failed"
internal const val NOT_SENT_RETRY = "chat.media.not_sent_retry"

/** What a long press on a bubble — or on any control inside one — is FOR, as
 *  TalkBack announces it ("double tap and hold to ..."). */
internal const val BUBBLE_ACTIONS_LABEL = "chat.message.actions"

/**
 * A control that lives INSIDE a bubble.
 *
 * `docs/ux/client-redesign.md`: the deliberate gesture reaches the action menu
 * from anywhere on the bubble, **including over the bubble's own controls**,
 * and a press long enough to be that gesture is not also a tap on the control
 * underneath it. Both halves are the rule, and the second is what makes the
 * first safe — a long press that opened the menu AND played the audio under it
 * would have done something nobody asked for.
 *
 * A plain `clickable` breaks both at once, and this is the whole of why: it
 * CONSUMES the press, so the ancestor Surface holding the menu's long press
 * never sees it, and it fires `onClick` on release whatever the press's
 * length. On Android that left a voice message reachable only by the padding no
 * control covered. Handing the control the menu's own callback fixes both from
 * one place: `combinedClickable` with a non-null `onLongClick` classifies the
 * gesture against `viewConfiguration.longPressTimeoutMillis` — the PLATFORM's
 * threshold, not iOS's 0.5 s, because a person's press is judged against the
 * long-press timeout their device taught them — and fires exactly one of the
 * two callbacks. So "not also a tap" is the detector's guarantee rather than a
 * duration comparison written here, and it survives a phone whose accessibility
 * settings lengthen that timeout.
 *
 * It also keeps what `clickable` was there for: the ripple, the [role], the
 * `onClick` semantics action, and now an `onLongClick` semantics action, so
 * TalkBack can reach the menu on a control too.
 *
 * `null` [onLongPress] means no menu is offered on this row at all (a deleted
 * or not-yet-acked message), and the control behaves exactly as it did before.
 */
@OptIn(ExperimentalFoundationApi::class)
internal fun Modifier.bubbleControl(
    onLongPress: (() -> Unit)?,
    onClickLabel: String? = null,
    role: Role? = null,
    onClick: () -> Unit,
): Modifier = combinedClickable(
    onClickLabel = onClickLabel,
    role = role,
    onClick = onClick,
    onLongClickLabel = if (onLongPress == null) null else Localization.strings.value[BUBBLE_ACTIONS_LABEL],
    onLongClick = onLongPress,
)

/**
 * One attachment in a bubble: a voice player, a round video note, an
 * animation, a photo or video (UX spec §10), or the plain file chip — chosen by [renderDecision], which needs
 * BOTH the sender's `kind` and a matching allowlisted MIME class (ADR-0047 §1).
 */
@Composable
internal fun AttachmentView(
    ref: AttachmentRef,
    hub: ChatMediaHub,
    isOwn: Boolean,
    contentColor: Color,
    /** The colour the bubble is drawn on. UX spec §2 wants the voice play
     *  button's glyph painted in it, so the button reads as a solid disc of
     *  the text colour on BOTH the sunken incoming surface and the accent
     *  outgoing one. Passed rather than re-derived from `isOwn` so it cannot
     *  drift from the Surface it actually sits on. */
    bubbleColor: Color,
    /** UX spec §10 — Save from the photo viewer / video player. */
    onSave: (AttachmentRef) -> Unit = {},
    /** The bubble's long-press menu, forwarded to EVERY bubble that carries a
     *  control of its own — their tap handlers would otherwise swallow the long
     *  press, which is exactly the defect `docs/ux/client-redesign.md` writes
     *  down; see [bubbleControl]. `null` when no menu is offered. */
    onLongPress: (() -> Unit)? = null,
    /** UX spec §10 — the time / ticks pill drawn on a photo or video. */
    mediaOverlay: (@Composable androidx.compose.foundation.layout.BoxScope.() -> Unit)? = null,
    /** UX spec §2 — the message's own meta line (time, edited marker, delivery
     *  ticks), drawn at the trailing end of the voice row's quiet line instead
     *  of on a row of its own under the bubble. Passed only when
     *  [me.lakko.app.media.bubbleMetaSlot] says
     *  [me.lakko.app.media.BubbleMetaSlot.VOICE_LINE]; `null` everywhere else,
     *  and ignored by every bubble but the voice one. */
    voiceMeta: (@Composable () -> Unit)? = null,
    fileChip: @Composable () -> Unit,
) {
    val decision = remember(ref.kind, ref.mime) { renderDecision(ref, hub.allowlist) }
    when (decision.bubble) {
        MediaBubble.VOICE -> VoiceBubble(ref, hub, isOwn, contentColor, bubbleColor, voiceMeta, onLongPress)
        MediaBubble.VIDEO_NOTE -> VideoNoteBubble(ref, hub, onLongPress)
        MediaBubble.GIF -> GifBubble(ref, hub, decision.inline, onLongPress)
        MediaBubble.PHOTO -> PhotoBubble(ref, hub, onSave, onLongPress, mediaOverlay)
        MediaBubble.VIDEO -> VideoBubble(ref, hub, onSave, onLongPress, mediaOverlay)
        MediaBubble.FILE -> fileChip()
    }
}

/** §2 timeline bubble — the play button's DRAWN diameter. */
private val VOICE_BUTTON_SIZE = 38.dp

/** The play button's TOUCH target. The spec draws the disc at 38dp to match
 *  iOS, which is under Android's 48dp minimum, so the target is stated
 *  separately and allowed to overflow the drawn disc's slot — see
 *  [VoicePlayDisc]. Neither size is a function of the other. */
private val VOICE_TOUCH_TARGET = 48.dp

/** §2 timeline bubble — the fixed width of the WAVEFORM beside the button.
 *  Fixed, not `weight(1f)`: a voice message is the same size whatever the
 *  bubble around it happens to be, and the waveform is a seek surface whose
 *  scale should not depend on the length of a neighbouring caption (the seek
 *  fraction is read off this canvas, so a varying width would move where a
 *  press lands in the audio).
 *
 *  The column AROUND it is `IntrinsicSize.Max` rather than this width, so the
 *  quiet line under the waveform — which since §2's "the message's own time
 *  sits on that same line" carries the clock, the speed control AND the
 *  message's meta — can take the room it needs on a long clock, a 12-hour
 *  time or a large font scale. Every child of that line either is always
 *  present or reserves its space, so the intrinsic is constant for a given
 *  message and the bubble still never resizes; and `width` coerces the
 *  intrinsic into the incoming constraints, so it can never push past
 *  `maxBubbleWidth`. */
private val VOICE_WAVEFORM_WIDTH = 170.dp

/** §2 timeline bubble — the waveform's height. */
private val VOICE_WAVEFORM_HEIGHT = 26.dp

/**
 * §2 timeline bubble.
 *
 * ONE row: a filled circle on the leading edge carrying the play / pause
 * glyph, then a column holding the waveform above one quiet line — the clock,
 * the unplayed dot, the speed control, and then [meta], the message's own time
 * and ticks, at the trailing end. The clock and the speed control belong to
 * that column, not to the bubble, so they line up with the waveform's edges
 * rather than with the button's.
 *
 * The upload ring and the failure line do not replace the row: the ring is
 * drawn around the play button with `requiredSize`, so it overflows the
 * button's 38dp slot without taking any layout space, and the failure line
 * goes under the row.
 *
 * **Nothing about the row's geometry changes, ever** — not between those states
 * and not when playback starts. Three children of the quiet line come and go
 * while a person watches the bubble, and all three RESERVE their space instead
 * of taking it when they appear: the speed control (absent until this message
 * is the one playing), the clock (`10:00` total becomes `0:01` elapsed on the
 * first frame of playback), and the unplayed dot (which vanishes the moment the
 * message is marked played). Reserving is not showing: each is absent to the
 * eye and to TalkBack until it has something to say.
 */
@Composable
internal fun VoiceBubble(
    ref: AttachmentRef,
    hub: ChatMediaHub,
    isOwn: Boolean,
    contentColor: Color,
    bubbleColor: Color,
    /** §2 — the message's own meta line at the trailing end of the quiet line.
     *  `null` for a voice message that is not alone in its bubble: that one has
     *  a paragraph to sit under and keeps the bubble's meta row. */
    meta: (@Composable () -> Unit)? = null,
    /** The bubble's action menu. Every control in this row takes it, so the
     *  deliberate gesture reaches the menu over the play disc, the waveform,
     *  the speed control and the failure line alike — see [bubbleControl]. */
    onLongPress: (() -> Unit)? = null,
) {
    val key = remember(ref.blobId) { ChatMediaHub.mediaKey(ref.blobId) }
    val voice by hub.voice.collectAsStateWithLifecycle()
    val playedVersion by hub.playedChanges.collectAsStateWithLifecycle()
    val requested by hub.requestedPlay.collectAsStateWithLifecycle()
    val knownDuration = ref.durationMs?.toLong() ?: 0L
    val isCurrent = voice.key == key
    val duration = if (isCurrent && voice.durationMs > 0) voice.durationMs else knownDuration
    val progress = if (isCurrent && duration > 0) voice.positionMs.toFloat() / duration else 0f
    val failed = voice.failedKey == key
    val unplayed = remember(playedVersion, key) { !isOwn && !hub.isPlayed(key) }

    // The voice bubble was the ONE media bubble that fetched nothing until it
    // was pressed: the photo, the GIF and the video note all start as soon as
    // they compose, and the list only composes what is on screen. So the first
    // press on a voice message paid a storage round trip and an AEAD open
    // before a single sample could be decoded — which is most of what "it sits
    // silent for a second and then plays" is. Fetch it like the others do; the
    // file is cached, so pressing play then finds it there.
    LaunchedEffect(key) {
        if (hub.fileFor(key) == null) hub.load(ref)
    }

    LaunchedEffect(requested) {
        if (requested == key) {
            hub.consumeRequest(key)
            hub.toggleVoice(ref, knownDuration, sentByMe = isOwn)
        }
    }

    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            VoicePlayDisc(
                playing = isCurrent && voice.playing,
                // §2 states — "Received, downloading": a spinner on first tap.
                loading = isCurrent && voice.loading,
                discColor = contentColor,
                glyphColor = bubbleColor,
                label = if (isCurrent && voice.playing) {
                    t("chat.voice.pause")
                } else {
                    t("chat.voice.play", mapOf("duration" to spokenDuration(duration)))
                },
                onClick = { hub.toggleVoice(ref, knownDuration, sentByMe = isOwn) },
                onLongPress = onLongPress,
            )
            Column(
                modifier = Modifier.width(IntrinsicSize.Max),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                WaveformSeekBar(
                    waveform = ref.waveform,
                    progress = progress,
                    playedColor = contentColor,
                    idleColor = contentColor.copy(alpha = 0.35f),
                    label = t("chat.voice.seek"),
                    modifier = Modifier.width(VOICE_WAVEFORM_WIDTH).height(VOICE_WAVEFORM_HEIGHT),
                    onLongPress = onLongPress,
                    onSeek = { f -> hub.seekVoice(ref, f, knownDuration, sentByMe = isOwn) },
                )
                VoiceQuietLine(
                    // The elapsed time while this message is the one playing —
                    // which means the one the player is on, not the one whose
                    // audio is moving. Reading `playing` here snapped the clock
                    // back to the total the moment the person paused, losing the
                    // place they paused at.
                    clock = if (isCurrent) formatClock(voice.positionMs) else formatTotalClock(duration),
                    clockReservation = voiceClockReservation(duration),
                    // Reserved on an incoming message, whose dot can vanish
                    // mid-playback; never on an own one, which has no dot to
                    // make room for.
                    dotSlot = !isOwn,
                    unplayed = unplayed,
                    // §2 — only while this message is the one playing: a speed
                    // control on a message nobody is listening to is a control
                    // for nothing.
                    showRate = isCurrent,
                    rate = voice.rate,
                    onCycleRate = hub::cycleRate,
                    contentColor = contentColor,
                    meta = meta,
                    onLongPress = onLongPress,
                )
            }
            // ADR-0082 / translation UX 4.1 — the transcript button at the
            // trailing end, in a slot that is part of the row in every state
            // (§2: the row never changes geometry).
            me.lakko.app.ui.translation.TranscriptSlot(ref, hub, contentColor, onLongPress)
        }
        // UX 4.1 / 4.2 — a gone file says so (also from the prefetch, before
        // any press) and offers no Retry; a limit offers Try again.
        val failures by hub.failures.collectAsStateWithLifecycle()
        val failure = failures[key]
        if (failed || failure == me.lakko.app.media.MediaFailure.GONE) {
            val retry = mediaRetryOffered(failure)
            MediaFailureLabel(
                failure = failure,
                generic = t(COULD_NOT_PLAY),
                onMedia = false,
                modifier = if (retry) {
                    Modifier.bubbleControl(
                        onClickLabel = t("common.retry"),
                        onLongPress = onLongPress,
                    ) { hub.toggleVoice(ref, knownDuration, sentByMe = isOwn) }
                } else {
                    Modifier
                },
            )
        }
        // UX 4.2 — the transcript opens BELOW the row, inside the same bubble.
        me.lakko.app.ui.translation.TranscriptUnder(ref, hub, contentColor)
    }
}

/**
 * §2 — the voice bubble's play button: a filled disc [VOICE_BUTTON_SIZE]
 * across in the bubble's text colour, its glyph in the bubble's own
 * background, so it reads as a solid button on the sunken incoming surface and
 * on the accent outgoing one alike.
 *
 * The disc is DRAWN at 38dp and TOUCHED at [VOICE_TOUCH_TARGET]: the
 * clickable sits on a box sized past the disc with `requiredSize`, which
 * ignores the 38dp slot's constraints, so the target overflows the slot and
 * costs no layout. The spec's geometry stays as written and the thumb still
 * gets its 48dp. (Compose also widens a small target's hit bounds on its own,
 * but that is a fallback applied when nothing was hit in bounds; this states
 * the 48dp rather than relying on it.)
 *
 * [uploadProgress] draws the determinate ring of the "Uploading" state AROUND
 * the disc by the same trick, so the row is the same size whether a message is
 * in flight or delivered.
 *
 * A `null` [onClick] makes it a decoration rather than a button, which is what
 * an in-flight message wants: it carries no semantics of its own, so TalkBack
 * reads the one the pending bubble already puts on the whole row.
 */
@Composable
private fun VoicePlayDisc(
    playing: Boolean,
    loading: Boolean,
    discColor: Color,
    glyphColor: Color,
    label: String?,
    onClick: (() -> Unit)?,
    onLongPress: (() -> Unit)? = null,
    uploadProgress: Float? = null,
) {
    Box(contentAlignment = Alignment.Center, modifier = Modifier.size(VOICE_BUTTON_SIZE)) {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .requiredSize(if (onClick == null) VOICE_BUTTON_SIZE else VOICE_TOUCH_TARGET)
                .then(
                    if (onClick == null) {
                        Modifier
                    } else {
                        // Clipped so the ripple is a circle around the disc
                        // rather than a 48dp square, as IconButton's is.
                        Modifier
                            .clip(CircleShape)
                            .bubbleControl(onLongPress = onLongPress, role = Role.Button, onClick = onClick)
                    },
                )
                .then(
                    if (label == null) Modifier else Modifier.semantics { contentDescription = label },
                ),
        ) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .size(VOICE_BUTTON_SIZE)
                    .clip(CircleShape)
                    .background(discColor),
            ) {
                if (loading) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(18.dp),
                        color = glyphColor,
                        strokeWidth = 2.dp,
                    )
                } else {
                    LkIcon(
                        icon = if (playing) R.drawable.ic_lk_pause_fill else R.drawable.ic_lk_play_fill,
                        // The disc carries the label; a second one here would
                        // make TalkBack read the button twice.
                        contentDescription = null,
                        tint = glyphColor,
                        size = LkIconSize.Row,
                    )
                }
            }
        }
        if (uploadProgress != null) {
            CircularProgressIndicator(
                progress = { uploadProgress.coerceIn(0f, 1f) },
                modifier = Modifier.requiredSize(VOICE_BUTTON_SIZE + 8.dp),
                color = discColor,
                strokeWidth = 3.dp,
                trackColor = Color.Transparent,
            )
        }
    }
}

/**
 * §2 — the one quiet line under the waveform: the clock, the unplayed dot, the
 * speed control, and the message's own [meta] at the trailing end.
 *
 * The geometry is fixed for a given message across its whole life. Three of its
 * children appear and disappear while the bubble is on screen, and none of them
 * is allowed to change the line's size when it does:
 *
 *  * the **clock** counts, and switches from the total to the elapsed time at
 *    the first frame of playback — it sits over a [clockReservation] twin, so a
 *    `10:00` message does not shrink to `0:01`'s width;
 *  * the **speed control** shows only while this message is the one playing —
 *    reserved at [widestRateLabel], measured rather than guessed at in dp, so a
 *    wider label or a bigger font scale still fits;
 *  * the **unplayed dot** vanishes when the message is marked played — its 6dp
 *    is held open on an incoming message ([dotSlot]) and never taken on an own
 *    one, which can never show it.
 *
 * Each reservation is laid out and drawn in no colour, with its semantics
 * cleared, so it is absent to the eye and to TalkBack: a speed control on a
 * message nobody is listening to would be a control for nothing.
 */
@Composable
private fun VoiceQuietLine(
    clock: String,
    clockReservation: String,
    dotSlot: Boolean,
    unplayed: Boolean,
    showRate: Boolean,
    rate: Float,
    onCycleRate: () -> Unit,
    contentColor: Color,
    meta: (@Composable () -> Unit)?,
    onLongPress: (() -> Unit)? = null,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Box(contentAlignment = Alignment.CenterStart) {
            VoiceClock(clockReservation, Color.Transparent, Modifier.clearAndSetSemantics {})
            VoiceClock(clock, contentColor.copy(alpha = 0.8f))
        }
        if (dotSlot) {
            Box(
                Modifier
                    .size(6.dp)
                    .then(
                        if (unplayed) {
                            Modifier
                                .clip(CircleShape)
                                .background(MaterialTheme.colorScheme.primary)
                                .semantics { contentDescription = Localization.strings.value["chat.voice.not_played"] }
                        } else {
                            Modifier
                        },
                    ),
            )
        }
        Spacer(Modifier.weight(1f))
        Box(contentAlignment = Alignment.CenterEnd) {
            VoiceRate(widestRateLabel(), Color.Transparent, Modifier.clearAndSetSemantics {})
            if (showRate) {
                VoiceRate(
                    text = rateLabel(rate),
                    color = contentColor,
                    modifier = Modifier
                        .clip(RoundedCornerShape(4.dp))
                        .bubbleControl(
                            onLongPress = onLongPress,
                            onClickLabel = t("chat.voice.change_speed"),
                            role = Role.Button,
                            onClick = onCycleRate,
                        )
                        .semantics {
                            contentDescription = Localization.strings.value.format(
                                "chat.voice.speed",
                                mapOf("rate" to rateLabel(rate)),
                            )
                        },
                )
            }
        }
        // §2 — "The message's own time sits on that same line, at its trailing
        // end", after the speed control, which keeps the trailing end of the
        // voice row's own controls. So the time lands where it lands in every
        // other bubble: last thing on the last line.
        meta?.invoke()
    }
}

/** §2 — the bubble's quiet clock. */
@Composable
private fun VoiceClock(text: String, color: Color, modifier: Modifier = Modifier) {
    Text(
        text = text,
        // Tabular figures, not a monospaced FACE: the digits stop shuffling as
        // they count, which is the point, while the colon keeps its own width.
        // `FontFamily.Monospace` gave the colon a full digit cell, so the
        // clock read "0 : 03".
        style = MaterialTheme.typography.labelSmall.copy(fontFeatureSettings = "tnum"),
        color = color,
        modifier = modifier,
    )
}

/** §2 — the speed control's label. The padding is INSIDE this, after the
 *  caller's modifier, so the real control's ripple covers it and the
 *  reservation twin measures to exactly the same size. */
@Composable
private fun VoiceRate(text: String, color: Color, modifier: Modifier = Modifier) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelSmall.copy(
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.Bold,
        ),
        color = color,
        modifier = modifier.padding(horizontal = 4.dp, vertical = 1.dp),
    )
}

/** §3 timeline bubble — a circle, no bubble chrome. */
@Composable
internal fun VideoNoteBubble(ref: AttachmentRef, hub: ChatMediaHub, onLongPress: (() -> Unit)? = null) {
    val scope = rememberCoroutineScope()
    val key = remember(ref.blobId) { ChatMediaHub.mediaKey(ref.blobId) }
    var poster by remember(key) { mutableStateOf<File?>(null) }
    var video by remember(key) { mutableStateOf<File?>(hub.fileFor(key)) }
    var loading by remember(key) { mutableStateOf(false) }
    var failed by remember(key) { mutableStateOf(false) }
    var playingWithSound by remember(key) { mutableStateOf(false) }
    var progress by remember(key) { mutableFloatStateOf(0f) }
    val autoplay = remember { hub.prefs.autoplayVideoNotes }
    val duration = ref.durationMs?.toLong() ?: 0L

    LaunchedEffect(key) {
        ref.thumb?.let { poster = hub.loadThumb(it) }
        if (autoplay && video == null) video = hub.load(ref)
    }
    DisposableEffect(key) {
        onDispose { hub.exclusive.release("video-$key") }
    }

    fun start() {
        if (failed && !mediaRetryOffered(hub.failureFor(key))) return
        failed = false
        if (video != null) {
            hub.exclusive.claim("video-$key") { playingWithSound = false }
            playingWithSound = true
            return
        }
        loading = true
        scope.launch {
            val f = hub.load(ref)
            loading = false
            if (f == null) {
                failed = true
            } else {
                video = f
                hub.exclusive.claim("video-$key") { playingWithSound = false }
                playingWithSound = true
            }
        }
    }

    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Box {
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(240.dp)
                .clip(CircleShape)
                .background(MaterialTheme.colorScheme.surfaceVariant)
                .bubbleControl(
                    onLongPress = onLongPress,
                    onClickLabel = if (playingWithSound) {
                        t("chat.record.pause_video")
                    } else {
                        t("chat.video_note.play", mapOf("duration" to spokenDuration(duration)))
                    },
                ) {
                    if (playingWithSound) {
                        playingWithSound = false
                        hub.exclusive.release("video-$key")
                    } else {
                        start()
                    }
                }
                .semantics {
                    contentDescription = Localization.strings.value.format(
                        "chat.media.with_duration",
                        mapOf(
                            "label" to Localization.strings.value["chat.media.video_message"],
                            "duration" to spokenDuration(duration),
                        ),
                    )
                },
        ) {
            val v = video
            val p = poster
            val showVideo = v != null && (playingWithSound || autoplay)
            if (v != null && (playingWithSound || autoplay)) {
                VideoSurface(
                    file = v,
                    playing = true,
                    muted = !playingWithSound,
                    looping = !playingWithSound,
                    modifier = Modifier.fillMaxSize(),
                    onProgress = { pos, dur -> if (dur > 0) progress = pos.toFloat() / dur },
                    onCompleted = {
                        playingWithSound = false
                        progress = 0f
                        hub.exclusive.release("video-$key")
                        hub.markPlayed(key)
                    },
                    onFailed = {
                        failed = true
                        playingWithSound = false
                    },
                )
            } else if (p != null) {
                // §3 states — a poster that fails to decode leaves the neutral
                // circle; never a broken-image icon.
                AnimatedImage(
                    source = AnimatedSource.OfFile(p),
                    contentDescription = t("chat.video_note.poster"),
                    modifier = Modifier.fillMaxSize(),
                )
            }
            when {
                loading -> CircularProgressIndicator(Modifier.size(40.dp), strokeWidth = 3.dp)
                !playingWithSound -> LkIcon(
                    icon = if (showVideo) R.drawable.ic_lk_speaker_slash_fill else R.drawable.ic_lk_play_fill,
                    contentDescription = null,
                    tint = Color.White,
                    modifier = Modifier
                        .size(44.dp)
                        .clip(CircleShape)
                        .background(Color.Black.copy(alpha = 0.4f))
                        .padding(8.dp),
                )
            }
            if (playingWithSound) {
                CircularProgressIndicator(
                    progress = { progress },
                    modifier = Modifier.fillMaxSize(),
                    strokeWidth = 3.dp,
                )
            }
        }
            // ADR-0082 / translation UX 4.1 — on the bubble's trailing bottom
            // edge, outside the circle, so the circle never moves.
            Box(
                Modifier
                    .align(Alignment.BottomEnd)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.surface.copy(alpha = 0.85f)),
            ) {
                me.lakko.app.ui.translation.TranscriptSlot(ref, hub, MaterialTheme.colorScheme.onSurface, onLongPress)
            }
        }
        // Outside any bubble colour: under the sender's own note the inherited
        // content colour was the bubble's white, unreadable on the chat
        // background (seen on a Pixel 7a, 2026-11-12).
        Text(
            formatTotalClock(duration),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        val failures by hub.failures.collectAsStateWithLifecycle()
        val failure = failures[key]
        if (failed || failure == me.lakko.app.media.MediaFailure.GONE) {
            MediaFailureLabel(failure, t(COULD_NOT_PLAY), onMedia = false)
        }
        Box(Modifier.widthIn(max = 240.dp)) {
            me.lakko.app.ui.translation.TranscriptUnder(ref, hub, MaterialTheme.colorScheme.onSurface)
        }
    }
}

/** Test tags of the GIF picture and of its label. */
internal const val GIF_IMAGE_TAG: String = "gif-image"
internal const val GIF_LABEL_TAG: String = "gif-label"

/** §5 timeline — the animation itself, looping and muted, sized to its aspect
 *  ratio, with a small `GIF` label under it (never on the picture); a tap
 *  opens it full-screen. */
@Composable
internal fun GifBubble(
    ref: AttachmentRef,
    hub: ChatMediaHub,
    inline: InlineKind?,
    onLongPress: (() -> Unit)? = null,
) {
    val key = remember(ref.blobId) { ChatMediaHub.mediaKey(ref.blobId) }
    var file by remember(key) { mutableStateOf<File?>(hub.fileFor(key)) }
    var failed by remember(key) { mutableStateOf(false) }
    var fullScreen by remember(key) { mutableStateOf(false) }
    var attempt by remember(key) { mutableStateOf(0) }
    LaunchedEffect(key, attempt) {
        if (file == null) {
            val f = hub.load(ref)
            if (f == null) failed = true else file = f
        }
    }
    val gifFailures by hub.failures.collectAsStateWithLifecycle()
    val gifFailure = if (failed) gifFailures[key] else null
    val w = ref.width?.toInt()?.takeIf { it > 0 }
    val h = ref.height?.toInt()?.takeIf { it > 0 }
    val ratio = if (w != null && h != null) (w.toFloat() / h).coerceIn(0.4f, 2.5f) else 1f
    // Base walk 2026-11-04 — the `GIF` label sits UNDER the picture, never on
    // it: drawn on the image it covered the GIF's own caption text.
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .testTag(GIF_IMAGE_TAG)
            .widthIn(max = 240.dp)
            .aspectRatio(ratio)
            .clip(RoundedCornerShape(10.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .bubbleControl(
                onLongPress = onLongPress,
                onClickLabel = if (failed) t("common.retry") else t("chat.gif.open_full"),
            ) {
                if (failed && !mediaRetryOffered(gifFailure)) {
                    Unit
                } else if (failed) {
                    failed = false
                    attempt += 1
                } else if (file != null) {
                    fullScreen = true
                }
            },
    ) {
        val f = file
        when {
            failed -> MediaFailureLabel(gifFailure, t(COULD_NOT_PLAY), onMedia = false)
            f == null -> CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 2.dp)
            else -> GifContent(f, inline, Modifier.fillMaxSize()) { failed = true }
        }
    }
    // media-messages.md §5 — a capsule at the leading edge, sunken surface,
    // muted 11/600 text.
    Text(
        t("chat.media.gif"),
        style = MaterialTheme.typography.labelSmall.copy(
            fontSize = 11.sp,
            fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold,
        ),
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier
            .testTag(GIF_LABEL_TAG)
            .clip(RoundedCornerShape(percent = 50))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .padding(horizontal = 8.dp, vertical = 2.dp),
    )
    }
    val f = file
    if (fullScreen && f != null) {
        Dialog(
            onDismissRequest = { fullScreen = false },
            properties = DialogProperties(usePlatformDefaultWidth = false),
        ) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .fillMaxSize()
                    .background(Color.Black)
                    .clickable(onClickLabel = t("common.close")) { fullScreen = false },
            ) {
                GifContent(f, inline, Modifier.fillMaxSize().aspectRatio(ratio, matchHeightConstraintsFirst = false)) {}
            }
        }
    }
}

@Composable
private fun GifContent(file: File, inline: InlineKind?, modifier: Modifier, onFailed: () -> Unit) {
    if (inline == InlineKind.VIDEO) {
        VideoSurface(file = file, playing = true, muted = true, looping = true, modifier = modifier, onFailed = onFailed)
    } else {
        AnimatedImage(
            source = AnimatedSource.OfFile(file),
            contentDescription = t("chat.media.gif"),
            modifier = modifier,
            onFailed = onFailed,
        )
    }
}

/**
 * An outgoing voice message, video note or GIF that has not reached the DS
 * yet: the bubble enters the timeline at once with a determinate upload ring
 * (§2 states "Uploading"), and a failure leaves it as `Not sent · Retry` —
 * nothing reached the other side.
 */
@Composable
internal fun PendingMediaBubble(
    row: PendingMediaRow,
    onRetry: () -> Unit,
    onDiscard: () -> Unit,
) {
    Column(horizontalAlignment = Alignment.End, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Surface(
            shape = if (row.kind == KIND_VIDEO_NOTE) CircleShape else RoundedCornerShape(12.dp),
            color = if (row.kind == KIND_VIDEO_NOTE) MaterialTheme.colorScheme.surfaceVariant else MaterialTheme.colorScheme.primaryContainer,
            contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
            modifier = Modifier.semantics {
                val strings = Localization.strings.value
                val kind = when (row.kind) {
                    KIND_VOICE -> strings.format(
                        "chat.media.with_duration",
                        mapOf(
                            "label" to strings["chat.media.voice_message"],
                            "duration" to spokenDuration(row.durationMs ?: 0L, strings),
                        ),
                    )
                    KIND_VIDEO_NOTE -> strings.format(
                        "chat.media.with_duration",
                        mapOf(
                            "label" to strings["chat.media.video_message"],
                            "duration" to spokenDuration(row.durationMs ?: 0L, strings),
                        ),
                    )
                    else -> strings["chat.media.gif"]
                }
                contentDescription = strings.format(
                    if (row.failed) "chat.media.row_not_sent" else "chat.media.row_sending",
                    mapOf("label" to kind),
                )
            },
        ) {
            val inner = when (row.kind) {
                KIND_VIDEO_NOTE -> Modifier.size(240.dp)
                KIND_GIF -> Modifier.size(width = 200.dp, height = 140.dp)
                // No fixed width: the voice row sizes itself exactly as the
                // delivered bubble's does, so nothing shifts when it lands.
                else -> Modifier.padding(horizontal = 12.dp, vertical = 10.dp)
            }
            Box(contentAlignment = Alignment.Center, modifier = inner) {
                when (row.kind) {
                    // §2 — the same row as a delivered voice message, with the
                    // determinate ring drawn around the play disc. Nothing
                    // about the geometry changes when the upload finishes.
                    KIND_VOICE -> Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        VoicePlayDisc(
                            playing = false,
                            loading = false,
                            discColor = MaterialTheme.colorScheme.onPrimaryContainer,
                            glyphColor = MaterialTheme.colorScheme.primaryContainer,
                            // The Surface above already describes the whole row.
                            label = null,
                            onClick = null,
                            uploadProgress = if (row.failed) null else row.progress,
                        )
                        Column(
                            modifier = Modifier.width(IntrinsicSize.Max),
                            verticalArrangement = Arrangement.spacedBy(4.dp),
                        ) {
                            WaveformSeekBar(
                                waveform = row.waveform,
                                progress = 0f,
                                playedColor = MaterialTheme.colorScheme.onPrimaryContainer,
                                idleColor = MaterialTheme.colorScheme.onPrimaryContainer.copy(alpha = 0.35f),
                                label = t("chat.media.voice_message"),
                                modifier = Modifier.width(VOICE_WAVEFORM_WIDTH).height(VOICE_WAVEFORM_HEIGHT),
                                onSeek = {},
                            )
                            // The same line as a delivered message's, so the
                            // height does not change when the row lands: the
                            // speed control's space is reserved here too, even
                            // though an in-flight message can never be playing.
                            // No dot (an own message has none) and no meta (the
                            // row has no DS time yet — it arrives with the
                            // delivered bubble, which is the one width change
                            // still left at the moment of landing).
                            VoiceQuietLine(
                                clock = formatTotalClock(row.durationMs ?: 0L),
                                clockReservation = voiceClockReservation(row.durationMs ?: 0L),
                                dotSlot = false,
                                unplayed = false,
                                showRate = false,
                                rate = 1f,
                                onCycleRate = {},
                                contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
                                meta = null,
                            )
                        }
                        // ADR-0082 — the transcript button's slot, empty here,
                        // so the row is as wide as the delivered one.
                        Spacer(Modifier.width(me.lakko.app.ui.translation.TRANSCRIPT_BUTTON_SLOT))
                    }
                    KIND_VIDEO_NOTE -> {
                        row.posterBytes?.let { bytes ->
                            AnimatedImage(
                                source = AnimatedSource.OfBytes("poster-${row.tempId}", bytes),
                                contentDescription = t("chat.video_note.poster"),
                                modifier = Modifier.fillMaxSize().clip(CircleShape),
                            )
                        }
                        UploadRing(row, Modifier.size(56.dp))
                    }
                    else -> {
                        row.previewBytes?.let { bytes ->
                            if (row.mime != "video/mp4") {
                                AnimatedImage(
                                    source = AnimatedSource.OfBytes("gif-${row.tempId}", bytes),
                                    contentDescription = t("chat.media.gif"),
                                    modifier = Modifier.fillMaxSize(),
                                )
                            }
                        }
                        UploadRing(row, Modifier.size(48.dp))
                    }
                }
            }
        }
        if (row.failed && row.limited) {
            // UX 4.2 — the sending limit, in place of the generic failure.
            Text(
                t("chat.attachment.limit.send"),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
        if (row.failed) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = onDiscard) {
                    Text(t("common.delete"), style = MaterialTheme.typography.labelSmall)
                }
                TextButton(onClick = onRetry) {
                    Text(
                        t(NOT_SENT_RETRY),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        }
    }
}

@Composable
private fun UploadRing(row: PendingMediaRow, modifier: Modifier) {
    Box(contentAlignment = Alignment.Center, modifier = modifier) {
        if (!row.failed) {
            CircularProgressIndicator(
                progress = { row.progress.coerceIn(0f, 1f) },
                modifier = Modifier.fillMaxSize(),
                strokeWidth = 3.dp,
            )
        }
    }
}

/**
 * UX spec §3 — "Autoplay video notes", default OFF: incoming clips autoplay
 * muted and looping only when this is on, because autoplay downloads media.
 * Takes effect for bubbles composed after the change.
 */
@Composable
public fun AutoplayVideoNotesSetting(modifier: Modifier = Modifier) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val prefs = remember { me.lakko.app.media.MediaPrefs.get(context) }
    var on by remember { mutableStateOf(prefs.autoplayVideoNotes) }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(Modifier.weight(1f)) {
            Text(me.lakko.app.l10n.t("settings.media.autoplay_video_notes.title"), style = MaterialTheme.typography.bodyLarge)
            Text(
                me.lakko.app.l10n.t("settings.media.autoplay_video_notes.sub"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        androidx.compose.material3.Switch(
            checked = on,
            onCheckedChange = { next ->
                on = next
                prefs.autoplayVideoNotes = next
            },
        )
    }
}
