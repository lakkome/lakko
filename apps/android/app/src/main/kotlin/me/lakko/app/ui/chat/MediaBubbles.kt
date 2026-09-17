// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.VolumeOff
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.InlineKind
import me.lakko.app.media.KIND_GIF
import me.lakko.app.media.KIND_VIDEO_NOTE
import me.lakko.app.media.KIND_VOICE
import me.lakko.app.media.MediaBubble
import me.lakko.app.media.PendingMediaRow
import me.lakko.app.media.formatClock
import me.lakko.app.media.rateLabel
import me.lakko.app.media.renderDecision
import me.lakko.app.media.spokenDuration
import uniffi.lakko_ffi_uniffi.AttachmentRef
import java.io.File

internal const val COULD_NOT_PLAY = "Couldn't play this message"
internal const val NOT_SENT_RETRY = "Not sent · Retry"

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
    /** UX spec §10 — Save from the photo viewer / video player. */
    onSave: (AttachmentRef) -> Unit = {},
    /** The bubble's long-press menu, forwarded by photo and video bubbles
     *  (their own tap handler would otherwise swallow the long press);
     *  `null` when no menu is offered. */
    onLongPress: (() -> Unit)? = null,
    /** UX spec §10 — the time / ticks pill drawn on a photo or video. */
    mediaOverlay: (@Composable androidx.compose.foundation.layout.BoxScope.() -> Unit)? = null,
    fileChip: @Composable () -> Unit,
) {
    val decision = remember(ref.kind, ref.mime) { renderDecision(ref, hub.allowlist) }
    when (decision.bubble) {
        MediaBubble.VOICE -> VoiceBubble(ref, hub, isOwn, contentColor)
        MediaBubble.VIDEO_NOTE -> VideoNoteBubble(ref, hub)
        MediaBubble.GIF -> GifBubble(ref, hub, decision.inline)
        MediaBubble.PHOTO -> PhotoBubble(ref, hub, onSave, onLongPress, mediaOverlay)
        MediaBubble.VIDEO -> VideoBubble(ref, hub, onSave, onLongPress, mediaOverlay)
        MediaBubble.FILE -> fileChip()
    }
}

/** §2 timeline bubble. */
@Composable
internal fun VoiceBubble(ref: AttachmentRef, hub: ChatMediaHub, isOwn: Boolean, contentColor: Color) {
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

    LaunchedEffect(requested) {
        if (requested == key) {
            hub.consumeRequest(key)
            hub.toggleVoice(ref, knownDuration, sentByMe = isOwn)
        }
    }

    Column(modifier = Modifier.widthIn(min = 200.dp, max = 260.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(contentAlignment = Alignment.Center, modifier = Modifier.size(40.dp)) {
                if (isCurrent && voice.loading) {
                    // §2 states — "Received, downloading": a spinner on first tap.
                    CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 2.dp)
                } else {
                    IconButton(onClick = { hub.toggleVoice(ref, knownDuration, sentByMe = isOwn) }) {
                        Icon(
                            imageVector = if (isCurrent && voice.playing) Icons.Filled.Pause else Icons.Filled.PlayArrow,
                            contentDescription = if (isCurrent && voice.playing) {
                                "Pause voice message"
                            } else {
                                "Play voice message, ${spokenDuration(duration)}"
                            },
                        )
                    }
                }
            }
            WaveformSeekBar(
                waveform = ref.waveform,
                progress = progress,
                playedColor = contentColor,
                idleColor = contentColor.copy(alpha = 0.35f),
                label = "Voice message position",
                modifier = Modifier.weight(1f).height(32.dp),
                onSeek = { f -> hub.seekVoice(ref, f, knownDuration, sentByMe = isOwn) },
            )
            if (unplayed) {
                Spacer(Modifier.width(4.dp))
                Box(
                    Modifier
                        .size(8.dp)
                        .clip(CircleShape)
                        .background(MaterialTheme.colorScheme.primary)
                        .semantics { contentDescription = "Not played" },
                )
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = formatClock(if (isCurrent && voice.playing) voice.positionMs else duration),
                style = MaterialTheme.typography.labelSmall,
                modifier = Modifier.padding(start = 8.dp),
            )
            Spacer(Modifier.weight(1f))
            if (isCurrent) {
                TextButton(onClick = hub::cycleRate) {
                    Text(
                        rateLabel(voice.rate),
                        style = MaterialTheme.typography.labelSmall,
                        modifier = Modifier.semantics { contentDescription = "Playback speed ${rateLabel(voice.rate)}" },
                    )
                }
            }
        }
        if (failed) {
            Text(
                text = COULD_NOT_PLAY,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier
                    .padding(start = 8.dp)
                    .clickable(onClickLabel = "Retry") { hub.toggleVoice(ref, knownDuration, sentByMe = isOwn) },
            )
        }
    }
}

/** §3 timeline bubble — a circle, no bubble chrome. */
@Composable
internal fun VideoNoteBubble(ref: AttachmentRef, hub: ChatMediaHub) {
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
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .size(240.dp)
                .clip(CircleShape)
                .background(MaterialTheme.colorScheme.surfaceVariant)
                .clickable(onClickLabel = if (playingWithSound) "Pause video message" else "Play video message") {
                    if (playingWithSound) {
                        playingWithSound = false
                        hub.exclusive.release("video-$key")
                    } else {
                        start()
                    }
                }
                .semantics { contentDescription = "Video message, ${spokenDuration(duration)}" },
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
                    contentDescription = "Video message poster",
                    modifier = Modifier.fillMaxSize(),
                )
            }
            when {
                loading -> CircularProgressIndicator(Modifier.size(40.dp), strokeWidth = 3.dp)
                !playingWithSound -> Icon(
                    imageVector = if (showVideo) Icons.AutoMirrored.Filled.VolumeOff else Icons.Filled.PlayArrow,
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
        Text(formatClock(duration), style = MaterialTheme.typography.labelSmall)
        if (failed) {
            Text(
                COULD_NOT_PLAY,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}

/** §5 timeline — the animation itself, looping and muted, sized to its aspect
 *  ratio, with a small `GIF` label; a tap opens it full-screen. */
@Composable
internal fun GifBubble(ref: AttachmentRef, hub: ChatMediaHub, inline: InlineKind?) {
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
    val w = ref.width?.toInt()?.takeIf { it > 0 }
    val h = ref.height?.toInt()?.takeIf { it > 0 }
    val ratio = if (w != null && h != null) (w.toFloat() / h).coerceIn(0.4f, 2.5f) else 1f
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .widthIn(max = 240.dp)
            .aspectRatio(ratio)
            .clip(RoundedCornerShape(10.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .clickable(onClickLabel = if (failed) "Retry" else "Open GIF") {
                if (failed) {
                    failed = false
                    attempt += 1
                } else if (file != null) {
                    fullScreen = true
                }
            },
    ) {
        val f = file
        when {
            failed -> Text(COULD_NOT_PLAY, style = MaterialTheme.typography.labelSmall)
            f == null -> CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 2.dp)
            else -> GifContent(f, inline, Modifier.fillMaxSize()) { failed = true }
        }
        Text(
            "GIF",
            style = MaterialTheme.typography.labelSmall,
            color = Color.White,
            modifier = Modifier
                .align(Alignment.BottomStart)
                .padding(6.dp)
                .clip(RoundedCornerShape(4.dp))
                .background(Color.Black.copy(alpha = 0.5f))
                .padding(horizontal = 4.dp, vertical = 1.dp),
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
                    .clickable(onClickLabel = "Close") { fullScreen = false },
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
            contentDescription = "GIF",
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
                contentDescription = when (row.kind) {
                    KIND_VOICE -> "Voice message, ${spokenDuration(row.durationMs ?: 0L)}"
                    KIND_VIDEO_NOTE -> "Video message, ${spokenDuration(row.durationMs ?: 0L)}"
                    else -> "GIF"
                } + if (row.failed) ", not sent" else ", sending"
            },
        ) {
            val inner = when (row.kind) {
                KIND_VIDEO_NOTE -> Modifier.size(240.dp)
                KIND_GIF -> Modifier.size(width = 200.dp, height = 140.dp)
                else -> Modifier.padding(horizontal = 12.dp, vertical = 10.dp).width(220.dp)
            }
            Box(contentAlignment = Alignment.Center, modifier = inner) {
                when (row.kind) {
                    KIND_VOICE -> Row(verticalAlignment = Alignment.CenterVertically) {
                        UploadRing(row, Modifier.size(36.dp))
                        Spacer(Modifier.width(8.dp))
                        WaveformSeekBar(
                            waveform = row.waveform,
                            progress = 0f,
                            playedColor = MaterialTheme.colorScheme.onPrimaryContainer,
                            idleColor = MaterialTheme.colorScheme.onPrimaryContainer.copy(alpha = 0.35f),
                            label = "Voice message",
                            modifier = Modifier.weight(1f).height(28.dp),
                            onSeek = {},
                        )
                        Spacer(Modifier.width(6.dp))
                        Text(formatClock(row.durationMs ?: 0L), style = MaterialTheme.typography.labelSmall)
                    }
                    KIND_VIDEO_NOTE -> {
                        row.posterBytes?.let { bytes ->
                            AnimatedImage(
                                source = AnimatedSource.OfBytes("poster-${row.tempId}", bytes),
                                contentDescription = "Video message poster",
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
                                    contentDescription = "GIF",
                                    modifier = Modifier.fillMaxSize(),
                                )
                            }
                        }
                        UploadRing(row, Modifier.size(48.dp))
                    }
                }
            }
        }
        if (row.failed) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = onDiscard) {
                    Text("Delete", style = MaterialTheme.typography.labelSmall)
                }
                TextButton(onClick = onRetry) {
                    Text(
                        NOT_SENT_RETRY,
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
            Text("Autoplay video notes", style = MaterialTheme.typography.bodyLarge)
            Text(
                "Plays incoming video messages muted as you scroll. Downloads media.",
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
