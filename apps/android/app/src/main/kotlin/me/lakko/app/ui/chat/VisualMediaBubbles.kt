// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.widget.MediaController
import android.widget.VideoView
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.PlayArrow
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
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.decodeOrientedBitmap
import me.lakko.app.media.mediaBubbleBox
import uniffi.lakko_ffi_uniffi.AttachmentRef
import java.io.File

internal const val COULD_NOT_LOAD_PHOTO = "Couldn't load photo"
internal const val COULD_NOT_LOAD_VIDEO = "Couldn't load video"

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

@OptIn(ExperimentalFoundationApi::class)
private fun Modifier.mediaGestures(
    label: String,
    onClick: () -> Unit,
    onLongPress: (() -> Unit)?,
): Modifier = combinedClickable(
    onClickLabel = label,
    onClick = onClick,
    onLongClick = onLongPress,
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
    Box(
        modifier = Modifier
            .size(box.widthDp.dp, box.heightDp.dp)
            .clip(MEDIA_SHAPE)
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .mediaGestures(
                label = if (failed) "Retry" else "Open photo",
                onClick = {
                    if (failed) {
                        failed = false
                        full = null
                        attempt += 1
                    } else if (ready) {
                        viewer = true
                    }
                },
                onLongPress = onLongPress,
            )
            .semantics { contentDescription = if (failed) "Photo, $COULD_NOT_LOAD_PHOTO" else "Photo" },
    ) {
        val f = file
        val fullBitmap = full
        val thumbBitmap = thumb
        when {
            !failed && animated && f != null -> AnimatedImage(
                source = AnimatedSource.OfFile(f),
                contentDescription = "Photo",
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
            FailedLabel(COULD_NOT_LOAD_PHOTO, Modifier.align(Alignment.Center))
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
    val sizeLabel = remember(ref.size) { AttachmentBridge.formatSize(ref.size) }
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

    Box(
        modifier = Modifier
            .size(box.widthDp.dp, box.heightDp.dp)
            .clip(MEDIA_SHAPE)
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .mediaGestures(
                label = if (failed) "Retry" else "Play video",
                onClick = ::open,
                onLongPress = onLongPress,
            )
            .semantics {
                contentDescription = if (failed) "Video, $COULD_NOT_LOAD_VIDEO" else "Video, $sizeLabel"
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
                Icon(
                    imageVector = Icons.Filled.PlayArrow,
                    contentDescription = null,
                    tint = Color.White,
                    modifier = Modifier.size(32.dp),
                )
            }
        }
        if (failed) {
            FailedLabel(
                COULD_NOT_LOAD_VIDEO,
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
            Icon(Icons.Filled.Close, contentDescription = "Close", tint = Color.White)
        }
        Box(Modifier.weight(1f))
        TextButton(onClick = onSave) { Text("Save", color = Color.White) }
    }
}

/**
 * UX spec §10 Viewer — full screen on black, pinch and double-tap zoom, pan
 * while zoomed, swipe down (at normal zoom) or Close to dismiss, Save through
 * the share sheet.
 */
@Composable
private fun PhotoViewer(file: File, animated: Boolean, onSave: () -> Unit, onClose: () -> Unit) {
    var bitmap by remember(file) { mutableStateOf<ImageBitmap?>(null) }
    var failed by remember(file) { mutableStateOf(false) }
    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(Offset.Zero) }
    var dragY by remember { mutableFloatStateOf(0f) }
    val dismissPx = with(LocalDensity.current) { 120.dp.toPx() }

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
                .semantics { contentDescription = "Photo viewer" },
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
                    contentDescription = "Photo",
                    modifier = content,
                    fit = true,
                    onFailed = { failed = true },
                )
                b != null -> Image(
                    bitmap = b,
                    contentDescription = "Photo",
                    contentScale = ContentScale.Fit,
                    modifier = content,
                )
                failed -> FailedLabel(COULD_NOT_LOAD_PHOTO, Modifier.align(Alignment.Center))
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
    Dialog(
        onDismissRequest = onClose,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(Color.Black)
                .semantics { contentDescription = "Video player" },
        ) {
            if (failed) {
                FailedLabel("Couldn't play this video", Modifier.align(Alignment.Center))
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
