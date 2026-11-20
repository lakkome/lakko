// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

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
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
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
import androidx.compose.ui.platform.LocalViewConfiguration
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import me.lakko.app.R
import me.lakko.app.l10n.t
import me.lakko.app.media.decodeOrientedBitmap
import me.lakko.app.ui.components.LkIcon
import java.io.File
import kotlin.math.abs

/**
 * One page of the media viewer (docs/ux/media-messages.md sections 10, 11):
 * a photo or a video of a message, or of the composer tray before sending.
 * [load] produces the decrypted (or local) file; it is asked only for the
 * page on screen and its neighbours.
 */
internal class ViewerItem(
    val key: String,
    val isVideo: Boolean,
    val animated: Boolean,
    val load: suspend () -> File?,
    /** Save for this item, `null` where there is none (the tray). */
    val save: (() -> Unit)?,
)

/** The position line, `2 / 5`; nothing for a single item. */
internal fun viewerPositionArgs(index: Int, total: Int): Map<String, String>? =
    if (total <= 1) null else mapOf("n" to (index + 1).toString(), "total" to total.toString())

/**
 * Section 11 — the full-screen viewer over several photos and videos: swipe
 * between them, the position line, Save for the one on screen, a video plays
 * when its page is shown. Pinch and double tap zoom a photo; a downward swipe
 * at normal zoom closes, as does Close.
 */
@Composable
internal fun MediaViewer(items: List<ViewerItem>, startIndex: Int, onClose: () -> Unit) {
    if (items.isEmpty()) return
    val pager = rememberPagerState(initialPage = startIndex.coerceIn(0, items.size - 1)) { items.size }
    val viewerLabel = t("chat.photo.viewer")
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Box(Modifier.fillMaxSize().background(Color.Black).semantics { contentDescription = viewerLabel }) {
            HorizontalPager(state = pager, modifier = Modifier.fillMaxSize(), beyondViewportPageCount = 1) { page ->
                val item = items[page]
                if (item.isVideo) {
                    ViewerVideoPage(item, current = pager.settledPage == page)
                } else {
                    ViewerPhotoPage(item, onClose)
                }
            }
            val current = items[pager.currentPage.coerceIn(0, items.size - 1)]
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .align(Alignment.TopCenter)
                    .fillMaxWidth()
                    .background(Color.Black.copy(alpha = 0.5f))
                    .statusBarsPadding()
                    .padding(horizontal = 4.dp),
            ) {
                IconButton(onClick = onClose) {
                    LkIcon(R.drawable.ic_lk_xmark, contentDescription = t("common.close"), tint = Color.White)
                }
                Box(Modifier.weight(1f), contentAlignment = Alignment.Center) {
                    viewerPositionArgs(pager.currentPage, items.size)?.let { args ->
                        Text(t("chat.viewer.position", args), color = Color.White, style = MaterialTheme.typography.titleSmall)
                    }
                }
                val save = current.save
                if (save != null) {
                    TextButton(onClick = save) { Text(t("common.save"), color = Color.White) }
                } else {
                    Box(Modifier.padding(horizontal = 24.dp))
                }
            }
        }
    }
}

@Composable
private fun ViewerPhotoPage(item: ViewerItem, onClose: () -> Unit) {
    var file by remember(item.key) { mutableStateOf<File?>(null) }
    var bitmap by remember(item.key) { mutableStateOf<ImageBitmap?>(null) }
    var failed by remember(item.key) { mutableStateOf(false) }
    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(Offset.Zero) }
    var dragY by remember { mutableFloatStateOf(0f) }
    val dismissPx = with(LocalDensity.current) { 120.dp.toPx() }
    val slop = LocalViewConfiguration.current.touchSlop
    LaunchedEffect(item.key) {
        val f = item.load()
        if (f == null) {
            failed = true
            return@LaunchedEffect
        }
        file = f
        if (!item.animated) {
            val decoded = withContext(Dispatchers.IO) { decodeOrientedBitmap(f, 2048)?.asImageBitmap() }
            if (decoded == null) failed = true else bitmap = decoded
        }
    }
    val content = Modifier
        .fillMaxSize()
        .pointerInput(item.key) {
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
        .pointerInput(item.key) {
            awaitEachGesture {
                awaitFirstDown(requireUnconsumed = false)
                var total = Offset.Zero
                var vertical = false
                do {
                    val event = awaitPointerEvent()
                    val pressed = event.changes.count { it.pressed }
                    val zoom = event.calculateZoom()
                    val pan = event.calculatePan()
                    if (scale <= 1f && pressed == 1 && zoom == 1f) {
                        // At normal zoom a sideways swipe belongs to the pager;
                        // a downward one closes.
                        total += pan
                        if (!vertical && abs(total.y) > slop && abs(total.y) > abs(total.x)) vertical = true
                        if (vertical) {
                            dragY = (dragY + pan.y).coerceAtLeast(0f)
                            event.changes.forEach { if (it.positionChanged()) it.consume() }
                        }
                    } else {
                        scale = (scale * zoom).coerceIn(1f, 5f)
                        offset = if (scale <= 1f) Offset.Zero else offset + pan
                        dragY = 0f
                        event.changes.forEach { if (it.positionChanged()) it.consume() }
                    }
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
    Box(Modifier.fillMaxSize()) {
        val f = file
        val b = bitmap
        when {
            failed -> ViewerFailed(t(COULD_NOT_LOAD_PHOTO), Modifier.align(Alignment.Center))
            item.animated && f != null -> AnimatedImage(
                source = AnimatedSource.OfFile(f),
                contentDescription = t("chat.media.photo"),
                modifier = content,
                fit = true,
                onFailed = { failed = true },
            )
            b != null -> Image(bitmap = b, contentDescription = t("chat.media.photo"), contentScale = ContentScale.Fit, modifier = content)
            else -> CircularProgressIndicator(Modifier.align(Alignment.Center), color = Color.White)
        }
    }
}

@Composable
private fun ViewerVideoPage(item: ViewerItem, current: Boolean) {
    var file by remember(item.key) { mutableStateOf<File?>(null) }
    var failed by remember(item.key) { mutableStateOf(false) }
    var requested by remember(item.key) { mutableStateOf(false) }
    if (current) requested = true
    LaunchedEffect(item.key, requested) {
        if (!requested || file != null) return@LaunchedEffect
        val f = item.load()
        if (f == null) failed = true else file = f
    }
    Box(Modifier.fillMaxSize()) {
        val f = file
        when {
            failed -> ViewerFailed(t(COULD_NOT_LOAD_VIDEO), Modifier.align(Alignment.Center))
            f != null && current -> AndroidView(
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
                        setVideoPath(f.absolutePath)
                    }
                },
                onRelease = { it.stopPlayback() },
                modifier = Modifier.align(Alignment.Center).fillMaxWidth(),
            )
            f != null -> LkIcon(
                R.drawable.ic_lk_play_fill,
                contentDescription = null,
                tint = Color.White,
                modifier = Modifier.align(Alignment.Center),
            )
            else -> CircularProgressIndicator(Modifier.align(Alignment.Center), color = Color.White)
        }
    }
}

@Composable
private fun ViewerFailed(text: String, modifier: Modifier) {
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
