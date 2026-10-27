// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.profile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.launch
import me.lakko.app.avatar.AvatarMedia
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.CROP_MAX_ZOOM
import me.lakko.app.profile.CROP_MIN_ZOOM
import me.lakko.app.profile.CropPosition
import me.lakko.app.profile.clampCrop
import me.lakko.app.profile.cropScale
import me.lakko.app.profile.cropSquare
import me.lakko.app.profile.moveCrop
import me.lakko.app.profile.videoWindow
import kotlin.math.roundToInt

/**
 * docs/ux/profile-screen.md §2.2 — choosing the square. The picture fills
 * the screen behind a circle; drag moves it, a pinch or the zoom slider
 * zooms between "fills the circle" and 4x. For a video the first frame is
 * shown still and a strip of frames under it chooses where the 10 s start.
 * `Use` encodes (photo: a square JPEG; video: a square MP4 without sound
 * plus its first frame); a video that cannot be encoded says so.
 */
@Composable
internal fun AvatarCropScreen(
    source: AvatarMedia.Source,
    onCancel: () -> Unit,
    onUse: (AvatarMedia.Encoded) -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val bitmap = when (source) {
        is AvatarMedia.Source.Photo -> source.bitmap
        is AvatarMedia.Source.Video -> source.firstFrame
    }
    val srcW = if (source is AvatarMedia.Source.Video) source.width else bitmap.width
    val srcH = if (source is AvatarMedia.Source.Video) source.height else bitmap.height
    val image = remember(bitmap) { bitmap.asImageBitmap() }
    var position by remember { mutableStateOf(CropPosition()) }
    var viewport by remember { mutableStateOf(0f) }
    var startMs by remember { mutableLongStateOf(0L) }
    var busy by remember { mutableStateOf(false) }
    var failure: UiText? by remember { mutableStateOf(null) }

    Dialog(
        onDismissRequest = { if (!busy) onCancel() },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Surface(color = Color.Black, contentColor = Color.White, modifier = Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().safeDrawingPadding()) {
                Text(
                    t("profile.crop.title"),
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.padding(16.dp),
                )
                BoxWithConstraints(
                    contentAlignment = Alignment.Center,
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth()
                        .pointerInput(srcW, srcH) {
                            detectTransformGestures { _, pan, zoom, _ ->
                                if (viewport > 0f) {
                                    position = moveCrop(srcW, srcH, viewport, position, pan.x, pan.y, zoom)
                                }
                            }
                        },
                ) {
                    val density = LocalDensity.current
                    val vp = with(density) { (minOf(maxWidth, maxHeight) * 0.85f).toPx() }
                    if (vp != viewport) {
                        viewport = vp
                        position = clampCrop(srcW, srcH, vp, position)
                    }
                    val scale = cropScale(srcW, srcH, vp, position.zoom)
                    Canvas(
                        modifier = Modifier
                            .fillMaxSize()
                            .graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen },
                    ) {
                        val w = srcW * scale
                        val h = srcH * scale
                        val left = size.width / 2f + position.offsetX - w / 2f
                        val top = size.height / 2f + position.offsetY - h / 2f
                        drawImage(
                            image = image,
                            srcOffset = IntOffset.Zero,
                            srcSize = IntSize(image.width, image.height),
                            dstOffset = IntOffset(left.roundToInt(), top.roundToInt()),
                            dstSize = IntSize(w.roundToInt().coerceAtLeast(1), h.roundToInt().coerceAtLeast(1)),
                        )
                        // The mask: everything outside the circle dimmed.
                        drawRect(Color.Black.copy(alpha = 0.6f))
                        drawCircle(
                            color = Color.Transparent,
                            radius = vp / 2f,
                            center = Offset(size.width / 2f, size.height / 2f),
                            blendMode = BlendMode.Clear,
                        )
                        drawCircle(
                            color = Color.White.copy(alpha = 0.8f),
                            radius = vp / 2f,
                            center = Offset(size.width / 2f, size.height / 2f),
                            style = Stroke(width = 2.dp.toPx()),
                        )
                    }
                }
                Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
                    val zoomLabel = t("profile.crop.zoom")
                    Text(zoomLabel, style = MaterialTheme.typography.labelMedium)
                    Slider(
                        value = position.zoom,
                        valueRange = CROP_MIN_ZOOM..CROP_MAX_ZOOM,
                        onValueChange = { z ->
                            if (viewport > 0f) {
                                position = moveCrop(srcW, srcH, viewport, position, 0f, 0f, z / position.zoom)
                            }
                        },
                        modifier = Modifier.semantics { contentDescription = zoomLabel },
                    )
                    if (source is AvatarMedia.Source.Video) {
                        StartStrip(source, startMs) { startMs = it }
                    }
                    failure?.let {
                        Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                        TextButton(enabled = !busy, onClick = onCancel, modifier = Modifier.weight(1f)) {
                            Text(t("profile.crop.cancel"), color = Color.White)
                        }
                        Button(
                            enabled = !busy && viewport > 0f,
                            onClick = {
                                val square = cropSquare(srcW, srcH, viewport, position)
                                busy = true
                                failure = null
                                scope.launch {
                                    val encoded = when (source) {
                                        is AvatarMedia.Source.Photo -> AvatarMedia.encodeStill(source.bitmap, square)
                                        is AvatarMedia.Source.Video -> {
                                            val (start, length) = videoWindow(source.durationMs, startMs)
                                            AvatarMedia.encodeVideo(context, source, square, start, length)
                                        }
                                    }
                                    busy = false
                                    if (encoded == null) {
                                        failure = if (source is AvatarMedia.Source.Video) {
                                            UiText("profile.video.unusable")
                                        } else {
                                            UiText("profile.save.error.generic")
                                        }
                                    } else {
                                        onUse(encoded)
                                    }
                                }
                            },
                            modifier = Modifier.weight(1f),
                        ) {
                            Text(if (busy) t("profile.save.saving") else t("profile.crop.use"))
                        }
                    }
                }
            }
        }
    }
}

/** The strip of frames that chooses where the 10 s start. */
@Composable
private fun StartStrip(source: AvatarMedia.Source.Video, startMs: Long, onStart: (Long) -> Unit) {
    val (start, length) = videoWindow(source.durationMs, startMs)
    Text(t("profile.crop.start"), style = MaterialTheme.typography.labelMedium)
    Spacer(Modifier.height(4.dp))
    LazyRow(horizontalArrangement = Arrangement.spacedBy(2.dp), modifier = Modifier.fillMaxWidth()) {
        itemsIndexed(source.strip) { _, (at, frame) ->
            val inWindow = at >= start && at < start + length
            val img = remember(frame) { frame.asImageBitmap() }
            Box(
                modifier = Modifier
                    .size(48.dp)
                    .background(Color.DarkGray)
                    .then(
                        if (inWindow) {
                            Modifier.border(BorderStroke(2.dp, MaterialTheme.colorScheme.primary))
                        } else {
                            Modifier.graphicsLayer { alpha = 0.5f }
                        },
                    )
                    .clickable { onStart(at) },
            ) {
                Image(img, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize())
            }
        }
    }
    Spacer(Modifier.height(8.dp))
}

