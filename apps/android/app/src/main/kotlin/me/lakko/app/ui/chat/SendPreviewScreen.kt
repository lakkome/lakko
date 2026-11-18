// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.InsertDriveFile
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Movie
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import me.lakko.app.l10n.t

/**
 * UX attachments sections 3-4 — the send preview: the current item large,
 * the strip of every item with a remove control, the caption, `Send as file`
 * (not for File), and `Send`. An item that cannot go says why and the rest
 * still send.
 */
@Composable
internal fun SendPreviewScreen(
    preview: SendPreview,
    onCaption: (String) -> Unit,
    onSendAsFile: (Boolean) -> Unit,
    onRemove: (Long) -> Unit,
    onSend: () -> Unit,
    onClose: () -> Unit,
) {
    var currentId by remember { mutableLongStateOf(preview.items.first().id) }
    val current = preview.items.firstOrNull { it.id == currentId } ?: preview.items.first()
    ContentEditorScaffold(
        title = t("attach.preview.title"),
        actionLabel = t("attach.preview.send"),
        actionEnabled = preview.items.any { it.problemKey == null },
        onAction = onSend,
        onClose = onClose,
    ) {
        Box(
            Modifier.fillMaxWidth().heightIn(min = 220.dp).clip(RoundedCornerShape(12.dp))
                .background(MaterialTheme.colorScheme.surfaceVariant),
            contentAlignment = Alignment.Center,
        ) {
            PreviewVisual(current, large = true)
        }
        current.problemKey?.let { key ->
            Text(t(key), color = MaterialTheme.colorScheme.error, fontSize = 13.sp)
        }
        if (preview.items.size > 1 || current.kind == PreviewKind.FILE) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
            ) {
                for (item in preview.items) {
                    Box {
                        Box(
                            Modifier.size(64.dp).clip(RoundedCornerShape(8.dp))
                                .background(MaterialTheme.colorScheme.surfaceVariant)
                                .then(
                                    if (item.id == current.id) {
                                        Modifier.border(2.dp, MaterialTheme.colorScheme.primary, RoundedCornerShape(8.dp))
                                    } else {
                                        Modifier
                                    },
                                )
                                .clickable { currentId = item.id },
                            contentAlignment = Alignment.Center,
                        ) { PreviewVisual(item, large = false) }
                        IconButton(onClick = { onRemove(item.id) }, modifier = Modifier.size(24.dp).align(Alignment.TopEnd)) {
                            Icon(
                                Icons.Filled.Close,
                                contentDescription = t("attach.preview.remove", mapOf("name" to (item.name ?: ""))),
                                modifier = Modifier.size(16.dp),
                            )
                        }
                    }
                }
            }
        }
        OutlinedTextField(
            value = preview.caption,
            onValueChange = onCaption,
            placeholder = { Text(t("attach.preview.caption")) },
            modifier = Modifier.fillMaxWidth(),
        )
        if (!preview.filesOnly) {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) {
                Text(t("attach.preview.send_as_file"), fontSize = 15.sp, modifier = Modifier.weight(1f))
                Switch(checked = preview.sendAsFile, onCheckedChange = onSendAsFile)
            }
        }
    }
}

@Composable
private fun PreviewVisual(item: PreviewItem, large: Boolean) {
    val bmp = remember(item.id) {
        item.thumb?.let { android.graphics.BitmapFactory.decodeByteArray(it, 0, it.size) }
    }
    if (bmp != null) {
        Image(
            bmp.asImageBitmap(),
            contentDescription = item.name,
            contentScale = if (large) ContentScale.Fit else ContentScale.Crop,
            modifier = if (large) Modifier.fillMaxWidth().heightIn(max = 360.dp) else Modifier.size(64.dp),
        )
    } else {
        Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.padding(8.dp)) {
            Icon(
                if (item.kind == PreviewKind.VIDEO) Icons.Filled.Movie else Icons.AutoMirrored.Filled.InsertDriveFile,
                contentDescription = null,
                modifier = Modifier.size(if (large) 48.dp else 24.dp),
            )
            if (large) {
                Text(item.name ?: "", fontSize = 13.sp, maxLines = 2)
                Text(android.text.format.Formatter.formatShortFileSize(androidx.compose.ui.platform.LocalContext.current, item.bytes.size.toLong()), fontSize = 12.sp)
            }
        }
    }
}
