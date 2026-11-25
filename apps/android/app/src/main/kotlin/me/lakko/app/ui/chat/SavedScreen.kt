// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.SavedRow
import me.lakko.app.chat.savedKindText
import me.lakko.app.chat.savedRows
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * docs/ux/saved-messages-and-file-lifetime.md section 3 — Saved: the
 * account's saved messages, most recently saved first. A row opens the chat
 * at the message; a long press offers Remove from saved. A message this
 * device does not hold yet is drawn from what was saved with it (no
 * thumbnail) and does not open until it is here.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun SavedScreen(
    sync: DeviceGroupSyncRepository,
    repository: ChatRepository,
    onBack: () -> Unit,
    /** Open [conversationHex] at [messageId]. */
    onOpenMessage: (conversationHex: String, messageId: Long) -> Unit,
    /** ADR-0087 UX 12 — a saved channel post: the channel's name, by id
     *  (hex), and opening the channel. */
    channelTitle: (channelIdHex: String) -> String? = { null },
    onOpenChannel: ((channelIdHex: String) -> Unit)? = null,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val saved by sync.saved.collectAsStateWithLifecycle()
    val chatState by repository.state.collectAsStateWithLifecycle()
    val directory = remember(repository) { ProfileDirectory.get(repository.context) }
    val rows = remember(saved, chatState) { savedRows(saved) { hex, id -> repository.heldMessage(hex, id) } }
    val hub = remember { me.lakko.app.media.ChatMediaHub(context, scope) }
    DisposableEffect(hub) { onDispose { hub.dispose() } }
    var error by remember { mutableStateOf(false) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(t("saved.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            if (error) {
                Text(
                    t("chat.error.save"),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
            if (rows.isEmpty()) {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        LkIcon(
                            R.drawable.ic_lk_bookmark,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            size = LkIconSize.Empty,
                        )
                        Spacer(Modifier.height(12.dp))
                        Text(
                            t("saved.empty"),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                return@Column
            }
            LazyColumn(Modifier.fillMaxSize()) {
                items(rows, key = { "${it.conversationHex}:${it.messageId}" }) { row ->
                    // The conversation that holds the row's segment (through
                    // the lineage): its title, and where the row opens.
                    val groupHex = remember(row.conversationHex, chatState) {
                        repository.conversationHolding(row.conversationHex)?.groupIdHex ?: row.conversationHex
                    }
                    val isChannel = row.conversation.kind == uniffi.lakko_ffi_uniffi.FolderConversationKind.CHANNEL
                    val title = remember(groupHex, chatState) {
                        if (isChannel) {
                            channelTitle(row.conversationHex) ?: row.author
                        } else {
                            conversationTitle(context, repository, directory, groupHex)
                        }
                    }
                    SavedRowView(
                        row = row,
                        title = title,
                        hub = hub,
                        // The jump names the row as the timeline shows it (an
                        // older segment's row has its own local id).
                        onOpen = {
                            if (isChannel) {
                                onOpenChannel?.invoke(row.conversationHex)
                            } else {
                                row.held?.let { onOpenMessage(groupHex, it.messageId) }
                            }
                        },
                        onRemove = {
                            scope.launch {
                                error = false
                                try {
                                    sync.applySavedChanges(
                                        listOf(
                                            uniffi.lakko_ffi_uniffi.composeSavedRemoval(
                                                me.lakko.app.devices.sync.Hlc.mint(),
                                                row.conversation,
                                                row.messageId.toULong(),
                                            ),
                                        ),
                                    )
                                } catch (e: Throwable) {
                                    me.lakko.app.rethrowIfCancelled(e)
                                    error = true
                                }
                            }
                        },
                    )
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SavedRowView(
    row: SavedRow,
    title: String,
    hub: me.lakko.app.media.ChatMediaHub,
    onOpen: () -> Unit,
    onRemove: () -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    val language = me.lakko.app.l10n.LocalStrings.current.language
    val date = remember(row.dateMs, language) {
        // Words from the language, order from the device (localization.md 5).
        me.lakko.app.l10n.DeviceFormats.mediumDate(row.dateMs, language)
    }
    Box {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .combinedClickable(onClick = onOpen, onLongClick = { menu = true })
                .padding(horizontal = 16.dp, vertical = 10.dp),
        ) {
            SavedThumb(row, hub)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        title,
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    Text(date, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                if (row.author.isNotEmpty()) {
                    Text(row.author, style = MaterialTheme.typography.labelMedium, maxLines = 1)
                }
                val kind = savedKindText(row.attachmentKind, row.excerpt)
                val line = when {
                    kind != null && (row.attachmentKind == "file" || row.excerpt.isEmpty()) -> kind.text()
                    kind != null -> "${kind.text()} · ${row.excerpt}"
                    else -> row.excerpt
                }
                if (line.isNotEmpty()) {
                    Text(
                        line,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
        if (menu) {
            me.lakko.app.ui.components.LakkoMenu(
                rows = listOf(me.lakko.app.ui.components.LakkoMenuRow(t("chat.message.unsave"), icon = null) { onRemove() }),
                onDismissRequest = { menu = false },
            )
        }
    }
}

/** The thumbnail of a held photo, video or video note (section 3); nothing
 *  for a row drawn from its snapshot. */
@Composable
private fun SavedThumb(row: SavedRow, hub: me.lakko.app.media.ChatMediaHub) {
    val ref = row.held?.attachments?.firstOrNull() ?: return
    val image by produceState<ImageBitmap?>(null, ref.blobId.contentHashCode()) {
        val file = ref.thumb?.let { hub.loadThumb(it) }
            ?: if (row.attachmentKind == "photo") hub.load(ref) else null
        value = file?.let { f ->
            withContext(Dispatchers.IO) { me.lakko.app.media.decodeOrientedBitmap(f, 160)?.asImageBitmap() }
        }
    }
    Box(
        Modifier
            .size(48.dp)
            .clip(RoundedCornerShape(8.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant),
    ) {
        image?.let { Image(it, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize()) }
    }
    Spacer(Modifier.width(12.dp))
}

