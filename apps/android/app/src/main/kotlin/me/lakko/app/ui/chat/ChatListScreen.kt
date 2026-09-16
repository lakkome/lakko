// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.text.format.DateFormat
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Archive
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.GroupAdd
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Unarchive
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ChatThread
import me.lakko.app.chat.Direction
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.devices.sync.FlagsState
import me.lakko.app.devices.sync.ReadCursorState
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.components.LakkoAvatar
import java.time.ZoneId
import java.util.Locale

/**
 * The conversation list — used twice, as the Chats tab and the Groups tab
 * (docs/ux/client-redesign.md section 5.1), filtered by the persisted
 * [ConversationKind].
 *
 * Receiving is automatic (the app-level long poll in `MainActivity`), so there
 * is no Refresh button; pull-to-refresh runs the same poll once. Row actions
 * live behind a swipe (pin to the right, archive to the left) and a long-press
 * menu (pin / mute / archive), never as permanent buttons in the row. The last
 * message and its time are read off the repository's persisted message log —
 * the thread already carries its messages; nothing about storage changes.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ChatListScreen(
    repository: ChatRepository,
    kind: ConversationKind,
    onOpenThread: (groupIdHex: String) -> Unit,
    /** New chat (Chats) or New group (Groups). */
    onCompose: () -> Unit,
    modifier: Modifier = Modifier,
    deviceGroupSync: DeviceGroupSyncRepository? = null,
) {
    val state by repository.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    // ADR-0048 UX §5 — a two-member chat is listed under the other person's
    // resolved name; a group under its local name or its members' names.
    val directory = remember(repository) { ProfileDirectory.get(repository.context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val flagsState = deviceGroupSync?.flags?.collectAsStateWithLifecycle()
    val flagsMap: Map<String, FlagsState> = flagsState?.value ?: emptyMap()
    val readCursorState = deviceGroupSync?.readCursors?.collectAsStateWithLifecycle()
    val readCursorMap: Map<String, ReadCursorState> =
        readCursorState?.value ?: emptyMap()
    var refreshing by remember { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }

    LaunchedEffect(Unit) {
        // One immediate poll so the list is fresh on arrival; the continuous
        // long poll across every group runs app-wide from `MainActivity`.
        repository.pollAllGroups()
    }

    val isChats = kind == ConversationKind.Chat
    val zone = remember { ZoneId.systemDefault() }
    val locale = remember { Locale.getDefault() }
    val use24h = remember(context) { DateFormat.is24HourFormat(context) }

    // Every row's presentation, derived once per state / name change: the
    // kind filter, the title, the preview line and the last-message time.
    val rows = remember(state, kind, nameVersion, flagsMap) {
        val now = System.currentTimeMillis()
        state.threads
            .filter { conversationKind(context, repository, it.groupIdHex) == kind }
            .map { thread ->
                val last = thread.messages.lastOrNull { it.direction != Direction.System }
                    ?: thread.messages.lastOrNull()
                val lastMs = last?.let { parseMessageTimeMs(it.createdAt) }
                val senderName = last?.takeIf { it.direction == Direction.Incoming }
                    ?.authorKey?.let { directory.resolve(it).primary }
                ThreadRowModel(
                    thread = thread,
                    title = conversationTitle(context, repository, directory, thread.groupIdHex),
                    preview = rowPreview(thread, last, isGroup = !isChats, senderName = senderName),
                    time = lastMs?.let { formatListTime(it, now, zone, use24h, locale) },
                    lastMs = lastMs ?: 0L,
                    flags = flagsMap[thread.groupIdHex],
                    unread = unreadCount(thread = thread, cursor = readCursorMap[thread.groupIdHex]),
                )
            }
    }
    val visible = remember(rows, query) {
        val q = query.trim()
        val matched = if (q.isEmpty()) {
            rows
        } else {
            rows.filter { it.title.contains(q, ignoreCase = true) || it.preview.contains(q, ignoreCase = true) }
        }
        // Pinned rows first, archived rows last, the newest message first within
        // each band.
        matched.sortedWith(
            compareByDescending<ThreadRowModel> { it.flags?.pinned == true }
                .thenBy { it.flags?.archived == true }
                .thenByDescending { it.lastMs },
        )
    }
    val synchronizing = remember(state) { repository.isSynchronizing() }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(if (isChats) "Chats" else "Groups") },
                actions = {
                    IconButton(onClick = onCompose) {
                        Icon(
                            imageVector = if (isChats) Icons.Filled.Edit else Icons.Filled.GroupAdd,
                            contentDescription = if (isChats) "New chat" else "New group",
                        )
                    }
                },
            )
        },
    ) { padding ->
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = {
                scope.launch {
                    refreshing = true
                    try {
                        repository.pollAllGroups()
                    } catch (_: Throwable) {
                        // The next automatic pass reports through the row state.
                    } finally {
                        refreshing = false
                    }
                }
            },
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            Column(modifier = Modifier.fillMaxSize()) {
                SearchField(
                    query = query,
                    onQueryChange = { query = it },
                    placeholder = if (isChats) "Search chats" else "Search groups",
                )
                // ADR-0050 / docs/ux/second-device.md section 2: one label while
                // this device is still being admitted to the account's chats.
                if (synchronizing) {
                    Text(
                        "Synchronizing...",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                    )
                }
                when {
                    rows.isEmpty() -> EmptyPanel(isChats = isChats, onCompose = onCompose)
                    visible.isEmpty() -> NoMatchPanel()
                    else -> LazyColumn(
                        modifier = Modifier.fillMaxSize(),
                        contentPadding = PaddingValues(vertical = 4.dp),
                    ) {
                        items(items = visible, key = { it.thread.groupIdHex }) { row ->
                            val toggle: (FlagField) -> Unit = { field ->
                                val sync = deviceGroupSync
                                val groupId = hexToBytes(row.thread.groupIdHex)
                                if (sync != null && groupId != null) {
                                    val cur = row.flags
                                    var pinned = cur?.pinned ?: false
                                    var muted = cur?.muted ?: false
                                    var archived = cur?.archived ?: false
                                    val unreadOverride = cur?.unreadOverride ?: false
                                    when (field) {
                                        FlagField.Pin -> pinned = !pinned
                                        FlagField.Mute -> muted = !muted
                                        FlagField.Archive -> archived = !archived
                                    }
                                    scope.launch {
                                        try {
                                            sync.setFlags(
                                                groupId = groupId,
                                                pinned = pinned,
                                                muted = muted,
                                                archived = archived,
                                                unreadOverride = unreadOverride,
                                            )
                                        } catch (cause: Throwable) {
                                            android.util.Log.w("ChatListScreen", "setFlags failed", cause)
                                        }
                                    }
                                }
                            }
                            SwipeableThreadRow(
                                row = row,
                                isGroup = !isChats,
                                onClick = { onOpenThread(row.thread.groupIdHex) },
                                onToggleFlag = toggle,
                            )
                        }
                    }
                }
            }
        }
    }
}

internal enum class FlagField { Pin, Mute, Archive }

/** Everything one list row renders, computed off the composition's hot path. */
internal data class ThreadRowModel(
    val thread: ChatThread,
    val title: String,
    val preview: String,
    /** The formatted last-message time, or `null` for no time. */
    val time: String?,
    /** Sort key: the last message's epoch millis, `0` when unknown. */
    val lastMs: Long,
    val flags: FlagsState?,
    val unread: Int,
)

/**
 * The second line of a row: the membership-ended line when this device's
 * membership is over (ADR-0045 UX §6.2.4), else the last message's preview
 * with its sender prefix, else "No messages yet".
 */
internal fun rowPreview(
    thread: ChatThread,
    last: ChatMessage?,
    isGroup: Boolean,
    senderName: String?,
): String {
    val endedLine = me.lakko.app.chat.chatListLine(
        me.lakko.app.chat.membershipState(
            isActive = !thread.membershipEnded,
            leaveRequestedAtMs = null,
        ),
    )
    if (endedLine != null) return endedLine
    if (last == null) return "No messages yet"
    if (last.direction == Direction.System) return last.text
    val line = previewLine(
        text = last.text,
        attachments = last.attachments.map { PreviewAttachment(it.kind, it.mime) },
        deleted = last.deleted,
        prefix = previewPrefix(last.direction, isGroup, senderName),
    )
    return line.ifEmpty { "No messages yet" }
}

/** Compute unread count for a thread.
 *
 *  Two sources, folded by [me.lakko.app.chat.combinedUnread]:
 *   - the cross-device read cursor (Phase 2a3e2c-followup): every incoming /
 *     system row whose `messageId` is strictly greater than the last broadcast
 *     cursor. Outgoing rows never count — the sender already "read" them by
 *     composing. `null` when the chat has no cursor.
 *   - the local count of incoming messages this device's polls applied while
 *     the chat was not on screen ([ChatThread.localUnread]). Without it a chat
 *     with no cursor never showed a badge, whatever arrived. */
internal fun unreadCount(thread: ChatThread, cursor: ReadCursorState?): Int {
    val cursorCount = cursor?.let { c ->
        val cursorId = decodeBeI64(c.upToMessageId)
        thread.messages.count { m -> m.direction != Direction.Outgoing && m.messageId > cursorId }
    }
    return me.lakko.app.chat.combinedUnread(
        cursorCount = cursorCount,
        local = me.lakko.app.chat.LocalUnread(
            count = thread.localUnread,
            viewedThisSession = thread.unreadViewedThisSession,
        ),
    )
}

private fun decodeBeI64(bytes: ByteArray): Long {
    if (bytes.size < 8) return Long.MAX_VALUE
    var v: Long = 0L
    for (i in 0 until 8) {
        v = (v shl 8) or (bytes[i].toLong() and 0xff)
    }
    return v
}

private fun hexToBytes(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    var i = 0
    while (i < hex.length) {
        val hi = Character.digit(hex[i], 16)
        val lo = Character.digit(hex[i + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i / 2] = ((hi shl 4) or lo).toByte()
        i += 2
    }
    return out
}

/** The search field under the app bar, filtering rows by name and preview. */
@Composable
private fun SearchField(query: String, onQueryChange: (String) -> Unit, placeholder: String) {
    OutlinedTextField(
        value = query,
        onValueChange = onQueryChange,
        placeholder = { Text(placeholder) },
        singleLine = true,
        leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
        trailingIcon = if (query.isNotEmpty()) {
            {
                IconButton(onClick = { onQueryChange("") }) {
                    Icon(Icons.Filled.Clear, contentDescription = "Clear search")
                }
            }
        } else {
            null
        },
        shape = RoundedCornerShape(12.dp),
        colors = OutlinedTextFieldDefaults.colors(
            focusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
            unfocusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
            focusedBorderColor = Color.Transparent,
            unfocusedBorderColor = Color.Transparent,
        ),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp),
    )
}

/**
 * A row with its swipe actions: right (start to end) pins or unpins, left (end
 * to start) archives or unarchives. The row always snaps back — the swipe is
 * a command, not a dismissal — so `confirmValueChange` refuses every settle.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SwipeableThreadRow(
    row: ThreadRowModel,
    isGroup: Boolean,
    onClick: () -> Unit,
    onToggleFlag: (FlagField) -> Unit,
) {
    val pinned = row.flags?.pinned == true
    val archived = row.flags?.archived == true
    val dismissState = rememberSwipeToDismissBoxState(
        confirmValueChange = { value ->
            when (value) {
                SwipeToDismissBoxValue.StartToEnd -> onToggleFlag(FlagField.Pin)
                SwipeToDismissBoxValue.EndToStart -> onToggleFlag(FlagField.Archive)
                SwipeToDismissBoxValue.Settled -> Unit
            }
            false
        },
        positionalThreshold = { total -> total * 0.35f },
    )
    SwipeToDismissBox(
        state = dismissState,
        backgroundContent = {
            val towardsEnd = dismissState.dismissDirection == SwipeToDismissBoxValue.StartToEnd
            val icon = if (towardsEnd) {
                Icons.Filled.PushPin
            } else if (archived) {
                Icons.Filled.Unarchive
            } else {
                Icons.Filled.Archive
            }
            val label = if (towardsEnd) {
                if (pinned) "Unpin" else "Pin"
            } else {
                if (archived) "Unarchive" else "Archive"
            }
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(MaterialTheme.colorScheme.primaryContainer)
                    .padding(horizontal = 24.dp),
                contentAlignment = if (towardsEnd) Alignment.CenterStart else Alignment.CenterEnd,
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        icon,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onPrimaryContainer,
                    )
                    Spacer(Modifier.width(8.dp))
                    Text(
                        label,
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onPrimaryContainer,
                    )
                }
            }
        },
    ) {
        Box(modifier = Modifier.background(MaterialTheme.colorScheme.background)) {
            ThreadRow(row = row, isGroup = isGroup, onClick = onClick, onToggleFlag = onToggleFlag)
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ThreadRow(
    row: ThreadRowModel,
    isGroup: Boolean,
    onClick: () -> Unit,
    onToggleFlag: (FlagField) -> Unit,
) {
    val thread = row.thread
    val archived = row.flags?.archived == true
    val pinned = row.flags?.pinned == true
    val muted = row.flags?.muted == true
    // ADR-0045 UX §6.2.4 / §6.2.11 — no badge once the membership has ended;
    // the count would be frozen for good.
    val unread = if (thread.membershipEnded) 0 else row.unread
    var menuOpen by remember { mutableStateOf(false) }
    Box {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .alpha(if (archived) 0.6f else 1f)
                .combinedClickable(onClick = onClick, onLongClick = { menuOpen = true })
                .padding(horizontal = 16.dp, vertical = 10.dp),
        ) {
            LakkoAvatar(name = row.title, seed = thread.groupIdHex, size = 40.dp, group = isGroup)
            Spacer(Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = row.title,
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = if (unread > 0) FontWeight.Bold else FontWeight.Medium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                    if (row.time != null) {
                        Spacer(Modifier.width(8.dp))
                        Text(
                            text = row.time,
                            style = MaterialTheme.typography.labelSmall,
                            color = if (unread > 0) {
                                MaterialTheme.colorScheme.primary
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                            maxLines = 1,
                        )
                    }
                }
                Spacer(Modifier.height(2.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = row.preview,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    if (muted) {
                        Spacer(Modifier.width(6.dp))
                        Icon(
                            Icons.Filled.NotificationsOff,
                            contentDescription = "Muted",
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.size(16.dp),
                        )
                    }
                    if (pinned) {
                        Spacer(Modifier.width(6.dp))
                        Icon(
                            Icons.Filled.PushPin,
                            contentDescription = "Pinned",
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.size(16.dp),
                        )
                    }
                    if (unread > 0) {
                        Spacer(Modifier.width(6.dp))
                        UnreadBadge(count = unread, muted = muted)
                    }
                }
            }
        }
        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
            DropdownMenuItem(
                text = { Text(if (pinned) "Unpin" else "Pin") },
                leadingIcon = { Icon(Icons.Filled.PushPin, contentDescription = null) },
                onClick = { menuOpen = false; onToggleFlag(FlagField.Pin) },
            )
            DropdownMenuItem(
                text = { Text(if (muted) "Unmute" else "Mute") },
                leadingIcon = { Icon(Icons.Filled.NotificationsOff, contentDescription = null) },
                onClick = { menuOpen = false; onToggleFlag(FlagField.Mute) },
            )
            DropdownMenuItem(
                text = { Text(if (archived) "Unarchive" else "Archive") },
                leadingIcon = {
                    Icon(
                        if (archived) Icons.Filled.Unarchive else Icons.Filled.Archive,
                        contentDescription = null,
                    )
                },
                onClick = { menuOpen = false; onToggleFlag(FlagField.Archive) },
            )
        }
    }
}

@Composable
private fun UnreadBadge(count: Int, muted: Boolean) {
    val label = if (count > 99) "99+" else count.toString()
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .clip(RoundedCornerShape(percent = 50))
            .background(
                if (muted) MaterialTheme.colorScheme.surfaceVariant
                else MaterialTheme.colorScheme.primary,
            )
            .padding(horizontal = 7.dp, vertical = 2.dp),
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = if (muted) MaterialTheme.colorScheme.onSurfaceVariant
            else MaterialTheme.colorScheme.onPrimary,
        )
    }
}

@Composable
private fun EmptyPanel(isChats: Boolean, onCompose: () -> Unit) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                text = if (isChats) "No chats yet" else "No groups yet",
                style = MaterialTheme.typography.titleMedium,
            )
            Spacer(Modifier.height(12.dp))
            Button(onClick = onCompose) {
                Text(if (isChats) "Start a chat" else "Create a group")
            }
        }
    }
}

@Composable
private fun NoMatchPanel() {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = "No matches",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
