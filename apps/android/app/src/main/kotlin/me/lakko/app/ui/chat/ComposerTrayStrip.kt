// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import me.lakko.app.R
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize
import java.io.File

/** Section 11 — the tray's thumbnail side, its corner radius, the (x) size. */
private val TRAY_TILE = 64.dp
private val TRAY_RADIUS = 8.dp
private val TRAY_REMOVE = 20.dp

/** The name an item is announced by: its file name, else its kind. */
@Composable
private fun trayItemLabel(item: TrayItem): String = item.name?.takeIf { it.isNotBlank() } ?: t(
    when (item.kind) {
        TrayKind.PHOTO -> "chat.media.photo"
        TrayKind.VIDEO -> "chat.media.video"
        TrayKind.FILE -> "chat.media.file"
    },
)

/**
 * docs/ux/media-messages.md section 11 — the strip of attached items directly
 * above the message field, in the order they were added. A photo or video is
 * a 64-dp square thumbnail (a video with its length), a file a chip; each has
 * its (x), a thin ring while it uploads and a retry glyph when its upload
 * failed. Tapping a photo or video opens the viewer over the tray's photos
 * and videos. The limit line stays under the strip for four seconds.
 */
@Composable
internal fun ComposerTrayStrip(
    tray: ComposerTray,
    onRemove: (Long) -> Unit,
    onRetry: (Long) -> Unit,
    localFile: (Long) -> File?,
) {
    var viewerAt by remember { mutableStateOf<Int?>(null) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    val notice = tray.notice
    LaunchedEffect(notice) {
        now = System.currentTimeMillis()
        if (notice != null) {
            delay((notice.atMs + TRAY_NOTICE_MS - now).coerceAtLeast(0))
            now = System.currentTimeMillis()
        }
    }
    Column(Modifier.fillMaxWidth().testTag("composer-tray")) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(start = 12.dp, end = 12.dp, top = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            val media = tray.media()
            for (item in tray.items) {
                if (item.isMedia) {
                    TrayMediaTile(
                        item = item,
                        onRemove = { onRemove(item.id) },
                        onRetry = { onRetry(item.id) },
                        onOpen = { viewerAt = media.indexOfFirst { it.id == item.id }.coerceAtLeast(0) },
                    )
                } else {
                    TrayFileChip(item = item, onRemove = { onRemove(item.id) }, onRetry = { onRetry(item.id) })
                }
            }
        }
        tray.noticeAt(now)?.let { text ->
            Text(
                text = text.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
            )
        }
    }
    viewerAt?.let { start ->
        val items = tray.media().map { item ->
            ViewerItem(
                key = "tray-${item.id}",
                isVideo = item.kind == TrayKind.VIDEO,
                animated = false,
                load = { localFile(item.id) },
                save = null,
            )
        }
        MediaViewer(items = items, startIndex = start, onClose = { viewerAt = null })
    }
}

@Composable
private fun TrayMediaTile(item: TrayItem, onRemove: () -> Unit, onRetry: () -> Unit, onOpen: () -> Unit) {
    val label = trayItemLabel(item)
    val failed = item.status == TrayStatus.Failed
    val bitmap = remember(item.thumb) { item.thumb?.let(::decodeTrayThumb) }
    val tapLabel = if (failed) t("chat.tray.retry", mapOf("name" to label)) else label
    Box(Modifier.size(TRAY_TILE)) {
        Box(
            modifier = Modifier
                .fillMaxSize()
                .clip(RoundedCornerShape(TRAY_RADIUS))
                .background(MaterialTheme.colorScheme.surfaceVariant)
                .clickable(role = Role.Button, onClickLabel = tapLabel) { if (failed) onRetry() else onOpen() }
                .semantics { contentDescription = tapLabel },
        ) {
            if (bitmap != null) {
                Image(bitmap, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize())
            }
            val duration = item.durationMs
            if (item.kind == TrayKind.VIDEO && duration != null) {
                Text(
                    text = me.lakko.app.media.formatClock(duration),
                    style = MaterialTheme.typography.labelSmall,
                    color = Color.White,
                    modifier = Modifier
                        .align(Alignment.BottomStart)
                        .padding(4.dp)
                        .clip(RoundedCornerShape(50))
                        .background(Color.Black.copy(alpha = 0.5f))
                        .padding(horizontal = 4.dp),
                )
            }
            TrayProgress(item, Modifier.align(Alignment.Center))
        }
        TrayRemoveButton(label, onRemove, Modifier.align(Alignment.TopEnd).padding(2.dp))
    }
}

@Composable
private fun TrayFileChip(item: TrayItem, onRemove: () -> Unit, onRetry: () -> Unit) {
    val label = trayItemLabel(item)
    val failed = item.status == TrayStatus.Failed
    val strings = LocalStrings.current
    val size = remember(item.size, strings) { if (item.size > 0) AttachmentBridge.formatSize(item.size.toULong(), strings) else "" }
    val tapLabel = if (failed) t("chat.tray.retry", mapOf("name" to label)) else label
    Box(Modifier.height(TRAY_TILE)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier
                .height(TRAY_TILE)
                .widthIn(min = 120.dp, max = 200.dp)
                .clip(RoundedCornerShape(TRAY_RADIUS))
                .background(MaterialTheme.colorScheme.surfaceVariant)
                .then(if (failed) Modifier.clickable(role = Role.Button, onClickLabel = tapLabel, onClick = onRetry) else Modifier)
                .semantics(mergeDescendants = true) { contentDescription = tapLabel }
                .padding(start = 10.dp, end = 26.dp),
        ) {
            Box(Modifier.size(28.dp), contentAlignment = Alignment.Center) {
                if (item.status == TrayStatus.Ready) {
                    LkIcon(R.drawable.ic_lk_doc, contentDescription = null, size = LkIconSize.Toolbar)
                } else {
                    TrayProgress(item, Modifier, onSurface = true)
                }
            }
            Column {
                Text(label, style = MaterialTheme.typography.labelLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                if (size.isNotEmpty()) {
                    Text(size, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
                }
            }
        }
        TrayRemoveButton(label, onRemove, Modifier.align(Alignment.TopEnd).padding(2.dp))
    }
}

/** The thin upload ring, or the retry glyph of a failed upload. */
@Composable
private fun TrayProgress(item: TrayItem, modifier: Modifier, onSurface: Boolean = false) {
    val color = if (onSurface) MaterialTheme.colorScheme.primary else Color.White
    val disc = if (onSurface) Modifier else Modifier.clip(CircleShape).background(Color.Black.copy(alpha = 0.45f)).padding(4.dp)
    when (val status = item.status) {
        TrayStatus.Ready -> Unit
        TrayStatus.Failed -> Box(modifier.then(disc)) {
            LkIcon(
                R.drawable.ic_lk_arrow_clockwise,
                contentDescription = null,
                tint = if (onSurface) MaterialTheme.colorScheme.error else Color.White,
                size = LkIconSize.Row,
            )
        }
        TrayStatus.Preparing, TrayStatus.Waiting -> CircularProgressIndicator(
            modifier = modifier.then(disc).size(20.dp),
            strokeWidth = 2.dp,
            color = color,
        )
        is TrayStatus.Uploading -> {
            val p = status.progress
            if (p == null) {
                CircularProgressIndicator(modifier = modifier.then(disc).size(20.dp), strokeWidth = 2.dp, color = color)
            } else {
                CircularProgressIndicator(progress = { p }, modifier = modifier.then(disc).size(20.dp), strokeWidth = 2.dp, color = color)
            }
        }
    }
}

/** The (x) in an item's top end corner, always visible on a touch screen. */
@Composable
private fun TrayRemoveButton(label: String, onRemove: () -> Unit, modifier: Modifier) {
    val removeLabel = t("chat.tray.remove", mapOf("name" to label))
    Box(
        contentAlignment = Alignment.Center,
        modifier = modifier
            .size(TRAY_REMOVE)
            .clip(CircleShape)
            .background(Color.Black.copy(alpha = 0.55f))
            .clickable(role = Role.Button, onClickLabel = removeLabel, onClick = onRemove)
            .semantics { contentDescription = removeLabel },
    ) {
        LkIcon(R.drawable.ic_lk_xmark, contentDescription = null, tint = Color.White, size = 12.dp)
    }
}

private fun decodeTrayThumb(bytes: ByteArray): ImageBitmap? = runCatching {
    android.graphics.BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
}.getOrNull()
