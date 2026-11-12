// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.runtime.collectAsState
import android.widget.MediaController
import android.widget.VideoView
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.decodeOrientedBitmap
import me.lakko.app.media.mediaBubbleBox
import uniffi.lakko_ffi_uniffi.AttachmentRef
import java.io.File
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

internal const val COULD_NOT_LOAD_PHOTO = "chat.photo.load_failed"
internal const val COULD_NOT_LOAD_VIDEO = "chat.video.load_failed"

/** Longest side, in px, a photo is decoded at inside its bubble. */
private const val BUBBLE_DECODE_SIDE = 1080

/** Longest side, in px, a photo is decoded at in the full-screen viewer. */
private const val VIEWER_DECODE_SIDE = 2048

private val MEDIA_SHAPE = RoundedCornerShape(18.dp)

/** GIF and WebP may animate; they go through the animated decoder, which
 *  shows every frame (API 28+). Everything else on the image allowlist is a
 *  still. */
private fun isAnimatedImage(mime: String?): Boolean {
    val essence = mime?.substringBefore(';')?.trim()?.lowercase() ?: return false
    return essence == "image/gif" || essence == "image/webp"
}

/**
 * The photo / video bubble's own tap, plus the bubble's long press.
 *
 * Delegates to [bubbleControl] rather than calling `combinedClickable` itself,
 * so the rule in `docs/ux/client-redesign.md` — a control answers a tap and
 * hands a longer press to the menu — has exactly ONE definition on this client.
 * These two bubbles were the only ones that ever took the callback; the defect
 * was everywhere it was not taken, and one helper is what keeps a bubble added
 * later from re-introducing it.
 */
private fun Modifier.mediaGestures(
    label: String,
    onClick: () -> Unit,
    onLongPress: (() -> Unit)?,
): Modifier = bubbleControl(
    onLongPress = onLongPress,
    onClickLabel = label,
    onClick = onClick,
)

/**
 * UX spec §10 — a photo in the timeline. Fixed size from `width` / `height`
 * before any download; the thumb first (blurred), then the full photo in
 * place, fetched as soon as the bubble is composed. The decrypted file lives
 * in the chat's media cache, so scrolling back does not download again.
 */
@Composable
internal fun PhotoBubble(
    ref: AttachmentRef,
    hub: ChatMediaHub,
    onSave: (AttachmentRef) -> Unit,
    onLongPress: (() -> Unit)?,
    overlay: (@Composable BoxScope.() -> Unit)?,
) {
    val key = remember(ref.blobId) { ChatMediaHub.mediaKey(ref.blobId) }
    val box = remember(ref.width, ref.height, ref.thumb) {
        mediaBubbleBox(
            ref.width?.toInt(),
            ref.height?.toInt(),
            ref.thumb?.width?.toInt(),
            ref.thumb?.height?.toInt(),
        )
    }
    val animated = remember(ref.mime) { isAnimatedImage(ref.mime) }
    var thumb by remember(key) { mutableStateOf<ImageBitmap?>(null) }
    var file by remember(key) { mutableStateOf<File?>(hub.fileFor(key)) }
    var full by remember(key) { mutableStateOf<ImageBitmap?>(null) }
    var failed by remember(key) { mutableStateOf(false) }
    var attempt by remember(key) { mutableIntStateOf(0) }
    var viewer by remember(key) { mutableStateOf(false) }

    LaunchedEffect(key) {
        val t = ref.thumb ?: return@LaunchedEffect
        if (file != null) return@LaunchedEffect
        val tf = hub.loadThumb(t) ?: return@LaunchedEffect
        thumb = withContext(Dispatchers.IO) { decodeOrientedBitmap(tf, 640)?.asImageBitmap() }
    }
    LaunchedEffect(key, attempt) {
        val f = file ?: hub.load(ref)
        if (f == null) {
            failed = true
            return@LaunchedEffect
        }
        file = f
        if (!animated) {
            val decoded = withContext(Dispatchers.IO) {
                decodeOrientedBitmap(f, BUBBLE_DECODE_SIDE)?.asImageBitmap()
            }
            if (decoded == null) failed = true else full = decoded
        }
    }

    val ready = file != null && (animated || full != null)
    // UX 4.1 / 4.2 — why it failed: gone has no Retry.
    val failures by hub.failures.collectAsState()
    val failure = if (failed) failures[key] else null
    val canRetry = mediaRetryOffered(failure)
    val photoLabel = when {
        failed && failure == me.lakko.app.media.MediaFailure.GONE -> t("chat.attachment.gone.title")
        failed -> t("chat.photo.load_failed_retry")
        else -> t("chat.media.photo")
    }
    Box(
        modifier = Modifier
            .size(box.widthDp.dp, box.heightDp.dp)
            .clip(MEDIA_SHAPE)
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .mediaGestures(
                label = t(if (failed) "common.retry" else "chat.photo.open"),
                onClick = {
                    if (failed && !canRetry) {
                        Unit
                    } else if (failed) {
                        failed = false
                        full = null
                        attempt += 1
                    } else if (ready) {
                        viewer = true
                    }
                },
                onLongPress = onLongPress,
            )
            .semantics { contentDescription = photoLabel },
    ) {
        val f = file
        val fullBitmap = full
        val thumbBitmap = thumb
        when {
            !failed && animated && f != null -> AnimatedImage(
                source = AnimatedSource.OfFile(f),
                contentDescription = t("chat.media.photo"),
                modifier = Modifier.fillMaxSize(),
                onFailed = { failed = true },
            )
            !failed && fullBitmap != null -> Image(
                bitmap = fullBitmap,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
            thumbBitmap != null -> Image(
                bitmap = thumbBitmap,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize().blur(6.dp),
            )
        }
        if (failed) {
            MediaFailureLabel(failure, t(COULD_NOT_LOAD_PHOTO), Modifier.align(Alignment.Center))
        } else if (!ready) {
            CircularProgressIndicator(
                strokeWidth = 2.dp,
                color = Color.White,
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .padding(8.dp)
                    .size(24.dp)
                    .clip(CircleShape)
                    .background(Color.Black.copy(alpha = 0.4f))
                    .padding(4.dp),
            )
        }
        overlay?.invoke(this)
    }

    val f = file
    if (viewer && f != null) {
        PhotoViewer(
            file = f,
            animated = animated,
            onSave = { onSave(ref) },
            onClose = { viewer = false },
        )
    }
}

/**
 * UX spec §10 — a video in the timeline: the poster with a play button and
 * the file size. Nothing is downloaded until the tap; then a progress ring on
 * the poster and the full-screen player.
 */
@Composable
internal fun VideoBubble(
    ref: AttachmentRef,
    hub: ChatMediaHub,
    onSave: (AttachmentRef) -> Unit,
    onLongPress: (() -> Unit)?,
    overlay: (@Composable BoxScope.() -> Unit)?,
) {
    val scope = rememberCoroutineScope()
    val key = remember(ref.blobId) { ChatMediaHub.mediaKey(ref.blobId) }
    val box = remember(ref.width, ref.height, ref.thumb) {
        mediaBubbleBox(
            ref.width?.toInt(),
            ref.height?.toInt(),
            ref.thumb?.width?.toInt(),
            ref.thumb?.height?.toInt(),
        )
    }
    val strings = LocalStrings.current
    val sizeLabel = remember(ref.size, strings) { AttachmentBridge.formatSize(ref.size, strings) }
    var poster by remember(key) { mutableStateOf<ImageBitmap?>(null) }
    var file by remember(key) { mutableStateOf<File?>(hub.fileFor(key)) }
    var loading by remember(key) { mutableStateOf(false) }
    var failed by remember(key) { mutableStateOf(false) }
    var player by remember(key) { mutableStateOf(false) }

    LaunchedEffect(key) {
        val t = ref.thumb ?: return@LaunchedEffect
        val pf = hub.loadThumb(t) ?: return@LaunchedEffect
        poster = withContext(Dispatchers.IO) { decodeOrientedBitmap(pf, 640)?.asImageBitmap() }
    }

    fun open() {
        if (loading) return
        if (failed && !mediaRetryOffered(hub.failureFor(key))) return
        failed = false
        if (file != null) {
            player = true
            return
        }
        loading = true
        scope.launch {
            val f = hub.load(ref)
            loading = false
            if (f == null) {
                failed = true
            } else {
                file = f
                player = true
            }
        }
    }

    // UX 4.1 / 4.2 — why it failed: gone has no Retry.
    val failures by hub.failures.collectAsState()
    val failure = if (failed) failures[key] else null
    val videoLabel = if (failed && failure == me.lakko.app.media.MediaFailure.GONE) {
        t("chat.attachment.gone.title")
    } else if (failed) {
        t("chat.video.load_failed_retry")
    } else {
        t("chat.video.play", mapOf("size" to sizeLabel))
    }
    Box(
        modifier = Modifier
            .size(box.widthDp.dp, box.heightDp.dp)
            .clip(MEDIA_SHAPE)
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .mediaGestures(
                label = t(if (failed) "common.retry" else "chat.video.open"),
                onClick = ::open,
                onLongPress = onLongPress,
            )
            .semantics {
                contentDescription = videoLabel
            },
    ) {
        poster?.let {
            Image(
                bitmap = it,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
        }
        Box(
            contentAlignment = Alignment.Center,
            modifier = Modifier
                .align(Alignment.Center)
                .size(52.dp)
                .clip(CircleShape)
                .background(Color.Black.copy(alpha = 0.45f)),
        ) {
            if (loading) {
                CircularProgressIndicator(Modifier.size(36.dp), strokeWidth = 3.dp, color = Color.White)
            } else {
                LkIcon(
                    icon = R.drawable.ic_lk_play_fill,
                    contentDescription = null,
                    tint = Color.White,
                    size = LkIconSize.Call,
                )
            }
        }
        if (failed) {
            MediaFailureLabel(
                failure,
                t(COULD_NOT_LOAD_VIDEO),
                Modifier.align(Alignment.BottomCenter).padding(bottom = 36.dp),
            )
        }
        Text(
            text = sizeLabel,
            style = MaterialTheme.typography.labelSmall,
            color = Color.White,
            modifier = Modifier
                .align(Alignment.TopStart)
                .padding(8.dp)
                .clip(RoundedCornerShape(50))
                .background(Color.Black.copy(alpha = 0.45f))
                .padding(horizontal = 8.dp, vertical = 2.dp),
        )
        overlay?.invoke(this)
    }

    val f = file
    if (player && f != null) {
        VideoPlayerDialog(file = f, onSave = { onSave(ref) }, onClose = { player = false })
    }
}

@Composable
private fun FailedLabel(text: String, modifier: Modifier) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelMedium,
        color = Color.White,
        modifier = modifier
            .clip(RoundedCornerShape(50))
            .background(Color.Black.copy(alpha = 0.55f))
            .padding(horizontal = 10.dp, vertical = 4.dp),
    )
}

/** The black top bar shared by the photo viewer and the video player. */
@Composable
private fun ViewerTopBar(onClose: () -> Unit, onSave: () -> Unit, modifier: Modifier = Modifier) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier
            .fillMaxWidth()
            .background(Color.Black.copy(alpha = 0.5f))
            .statusBarsPadding()
            .padding(horizontal = 4.dp),
    ) {
        IconButton(onClick = onClose) {
            LkIcon(R.drawable.ic_lk_xmark, contentDescription = t("common.close"), tint = Color.White)
        }
        Box(Modifier.weight(1f))
        TextButton(onClick = onSave) { Text(t("common.save"), color = Color.White) }
    }
}

/**
 * UX spec §10 Viewer — full screen on black, pinch and double-tap zoom, pan
 * while zoomed, swipe down (at normal zoom) or Close to dismiss, Save through
 * the system's save picker.
 */
@Composable
private fun PhotoViewer(file: File, animated: Boolean, onSave: () -> Unit, onClose: () -> Unit) {
    var bitmap by remember(file) { mutableStateOf<ImageBitmap?>(null) }
    var failed by remember(file) { mutableStateOf(false) }
    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(Offset.Zero) }
    var dragY by remember { mutableFloatStateOf(0f) }
    val dismissPx = with(LocalDensity.current) { 120.dp.toPx() }
    val viewerLabel = t("chat.photo.viewer")

    LaunchedEffect(file) {
        if (animated) return@LaunchedEffect
        val decoded = withContext(Dispatchers.IO) {
            decodeOrientedBitmap(file, VIEWER_DECODE_SIDE)?.asImageBitmap()
        }
        if (decoded == null) failed = true else bitmap = decoded
    }

    Dialog(
        onDismissRequest = onClose,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black)
                .semantics { contentDescription = viewerLabel },
        ) {
            val content = Modifier
                .fillMaxSize()
                .pointerInput(Unit) {
                    detectTapGestures(
                        onDoubleTap = {
                            if (scale > 1f) {
                                scale = 1f
                                offset = Offset.Zero
                            } else {
                                scale = 2.5f
                            }
                        },
                    )
                }
                .pointerInput(Unit) {
                    awaitEachGesture {
                        awaitFirstDown(requireUnconsumed = false)
                        do {
                            val event = awaitPointerEvent()
                            val pressed = event.changes.count { it.pressed }
                            val zoom = event.calculateZoom()
                            val pan = event.calculatePan()
                            if (scale <= 1f && pressed == 1 && zoom == 1f) {
                                dragY = (dragY + pan.y).coerceAtLeast(0f)
                            } else {
                                scale = (scale * zoom).coerceIn(1f, 5f)
                                offset = if (scale <= 1f) Offset.Zero else offset + pan
                                dragY = 0f
                            }
                            event.changes.forEach { if (it.positionChanged()) it.consume() }
                        } while (event.changes.any { it.pressed })
                        if (dragY > dismissPx) onClose() else dragY = 0f
                    }
                }
                .graphicsLayer {
                    scaleX = scale
                    scaleY = scale
                    translationX = offset.x
                    translationY = offset.y + dragY
                    alpha = 1f - (dragY / (dismissPx * 3)).coerceIn(0f, 0.6f)
                }
            val b = bitmap
            when {
                animated -> AnimatedImage(
                    source = AnimatedSource.OfFile(file),
                    contentDescription = t("chat.media.photo"),
                    modifier = content,
                    fit = true,
                    onFailed = { failed = true },
                )
                b != null -> Image(
                    bitmap = b,
                    contentDescription = t("chat.media.photo"),
                    contentScale = ContentScale.Fit,
                    modifier = content,
                )
                failed -> FailedLabel(t(COULD_NOT_LOAD_PHOTO), Modifier.align(Alignment.Center))
                else -> CircularProgressIndicator(Modifier.align(Alignment.Center), color = Color.White)
            }
            ViewerTopBar(onClose = onClose, onSave = onSave, modifier = Modifier.align(Alignment.TopCenter))
        }
    }
}

/**
 * UX spec §10 Viewer — a video full screen with the platform's standard
 * controls (`VideoView` + `MediaController`, no extra dependency).
 */
@Composable
private fun VideoPlayerDialog(file: File, onSave: () -> Unit, onClose: () -> Unit) {
    var failed by remember(file) { mutableStateOf(false) }
    val playerLabel = t("chat.video.player")
    Dialog(
        onDismissRequest = onClose,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black)
                .semantics { contentDescription = playerLabel },
        ) {
            if (failed) {
                FailedLabel(t("chat.video.play_failed"), Modifier.align(Alignment.Center))
            } else {
                AndroidView(
                    factory = { ctx ->
                        VideoView(ctx).apply {
                            val controller = MediaController(ctx)
                            controller.setAnchorView(this)
                            setMediaController(controller)
                            setOnErrorListener { _, _, _ ->
                                failed = true
                                true
                            }
                            setOnPreparedListener { start() }
                            setVideoPath(file.absolutePath)
                        }
                    },
                    onRelease = { it.stopPlayback() },
                    modifier = Modifier.align(Alignment.Center).fillMaxWidth(),
                )
            }
            ViewerTopBar(onClose = onClose, onSave = onSave, modifier = Modifier.align(Alignment.TopCenter))
        }
    }
}
