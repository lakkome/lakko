// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.DragHandle
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.zIndex
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.chat.ChatFolder
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.FolderActions
import me.lakko.app.chat.FolderLimits
import me.lakko.app.chat.rowsMatching
import me.lakko.app.chat.suggestionsNotHeld
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text

/**
 * Settings > Chat folders, `docs/ux/chat-folders.md` section 4: the folders
 * in bar order with their match counts (long press and drag to reorder, with
 * TalkBack's Move up / Move down as the non-drag equivalent), New folder, the
 * suggestions the person does not already have, and the one line of this
 * feature that mentions the other devices.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ChatFoldersSettingsScreen(
    repository: ChatRepository,
    channels: ChannelsUiState?,
    deviceGroupSync: DeviceGroupSyncRepository,
    onBack: () -> Unit,
    onEditFolder: (folderIdHex: String) -> Unit,
    onNewFolder: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val folderViews by deviceGroupSync.folders.collectAsStateWithLifecycle()
    val folders = remember(folderViews) { folderViews.map(ChatFolder::of) }
    val pending by deviceGroupSync.pendingSync.collectAsStateWithLifecycle()
    val actions = remember(deviceGroupSync, repository) { FolderActions(deviceGroupSync, repository) }
    val allRows = rememberConversationRows(repository, channels, deviceGroupSync)
    val counts = remember(folders, allRows) {
        folders.associate { it.idHex to rowsMatching(it.rule, allRows).size }
    }
    val suggestions = remember(folders) { suggestionsNotHeld(folders) }
    val atLimit = folders.size >= FolderLimits.MAX_FOLDERS

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(t("settings.section.folders")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState()),
        ) {
            // UX spec 6 — while anything written here is still waiting.
            if (pending) {
                Muted(t("folders.settings.pending_sync"))
            }
            if (folders.isEmpty()) {
                Muted(t("folders.settings.intro"))
            } else {
                Label(t("folders.settings.yours"))
                // All chats: first and fixed, no handle.
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 14.dp),
                ) {
                    Text(t("folders.all_chats"), style = MaterialTheme.typography.bodyLarge)
                }
                ReorderableFolders(
                    folders = folders,
                    counts = counts,
                    onOpen = { onEditFolder(it.idHex) },
                    onReorder = { next -> scope.launch { actions.reorder(next) } },
                )
            }

            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(enabled = !atLimit, onClick = onNewFolder)
                    .padding(horizontal = 16.dp, vertical = 14.dp),
            ) {
                val tint = if (atLimit) {
                    MaterialTheme.colorScheme.onSurfaceVariant
                } else {
                    MaterialTheme.colorScheme.primary
                }
                Icon(Icons.Filled.Add, contentDescription = null, tint = tint)
                Spacer(Modifier.width(16.dp))
                Text(t("folders.new"), color = tint, style = MaterialTheme.typography.bodyLarge)
            }
            if (atLimit) {
                Muted(t("folders.limit", emptyMap(), mapOf("count" to FolderLimits.MAX_FOLDERS.toULong())))
            }

            if (suggestions.isNotEmpty()) {
                Label(t("folders.suggested.title"))
                for (s in suggestions) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(start = 16.dp, end = 8.dp, top = 6.dp, bottom = 6.dp),
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text(s.name.text(), style = MaterialTheme.typography.bodyLarge)
                            Text(
                                s.description.text(),
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        val addA11y = t("folders.suggested.add_a11y", mapOf("name" to s.name.text()))
                        TextButton(
                            enabled = !atLimit,
                            onClick = { scope.launch { actions.addSuggestion(s) } },
                            modifier = Modifier.semantics { contentDescription = addA11y },
                        ) { Text(t("folders.suggested.add")) }
                    }
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

/**
 * The folders as rows that a long press lifts and a drag moves (UX spec 4,
 * Android). Twenty rows at most, so a plain column with measured heights is
 * enough. The new order is written once, when the finger lifts.
 */
@Composable
private fun ReorderableFolders(
    folders: List<ChatFolder>,
    counts: Map<String, Int>,
    onOpen: (ChatFolder) -> Unit,
    onReorder: (List<ChatFolder>) -> Unit,
) {
    var order by remember(folders) { mutableStateOf(folders) }
    var dragging by remember { mutableStateOf<String?>(null) }
    var dragOffset by remember { mutableFloatStateOf(0f) }
    val heights = remember { mutableStateMapOf<String, Int>() }
    var startIndex by remember { mutableIntStateOf(-1) }

    val moveUp = t("folders.settings.move_up")
    val moveDown = t("folders.settings.move_down")

    fun move(from: Int, to: Int): List<ChatFolder> {
        val list = order.toMutableList()
        val item = list.removeAt(from)
        list.add(to, item)
        return list
    }

    Column {
        order.forEachIndexed { index, folder ->
            val id = folder.idHex
            // Keyed, so a row being dragged keeps its gesture when it moves.
            key(id) {
            val isDragging = dragging == id
            val count = counts[id] ?: 0
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .zIndex(if (isDragging) 1f else 0f)
                    .graphicsLayer { translationY = if (isDragging) dragOffset else 0f }
                    .background(
                        if (isDragging) {
                            MaterialTheme.colorScheme.surfaceVariant
                        } else {
                            MaterialTheme.colorScheme.background
                        },
                    )
                    .fillMaxWidth()
                    .onGloballyPositioned { heights[id] = it.size.height }
                    .semantics {
                        customActions = listOfNotNull(
                            if (index > 0) {
                                CustomAccessibilityAction(moveUp) {
                                    order = move(index, index - 1)
                                    onReorder(order)
                                    true
                                }
                            } else {
                                null
                            },
                            if (index < order.size - 1) {
                                CustomAccessibilityAction(moveDown) {
                                    order = move(index, index + 1)
                                    onReorder(order)
                                    true
                                }
                            } else {
                                null
                            },
                        )
                    }
                    .pointerInput(id) {
                        detectDragGesturesAfterLongPress(
                            onDragStart = {
                                dragging = id
                                dragOffset = 0f
                                startIndex = order.indexOfFirst { it.idHex == id }
                            },
                            onDrag = { change, amount ->
                                change.consume()
                                dragOffset += amount.y
                                val i = order.indexOfFirst { it.idHex == id }
                                if (dragOffset > 0 && i < order.size - 1) {
                                    val below = heights[order[i + 1].idHex] ?: 0
                                    if (below > 0 && dragOffset > below / 2f) {
                                        order = move(i, i + 1)
                                        dragOffset -= below
                                    }
                                } else if (dragOffset < 0 && i > 0) {
                                    val above = heights[order[i - 1].idHex] ?: 0
                                    if (above > 0 && -dragOffset > above / 2f) {
                                        order = move(i, i - 1)
                                        dragOffset += above
                                    }
                                }
                            },
                            onDragEnd = {
                                val end = order.indexOfFirst { it.idHex == id }
                                dragging = null
                                dragOffset = 0f
                                if (end != startIndex) onReorder(order)
                            },
                            onDragCancel = {
                                dragging = null
                                dragOffset = 0f
                                order = folders
                            },
                        )
                    }
                    .clickable { onOpen(folder) }
                    .padding(horizontal = 16.dp, vertical = 10.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(folder.name, style = MaterialTheme.typography.bodyLarge)
                    Text(
                        chatsCount(count),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Icon(
                    Icons.Filled.DragHandle,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            }
        }
    }
}

@Composable
private fun Label(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun Muted(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
    )
}
