// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

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
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.GroupAdd
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ChatThread
import me.lakko.app.chat.Direction
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.devices.sync.FlagsState
import me.lakko.app.devices.sync.ReadCursorState
import me.lakko.app.ui.components.LakkoAvatar

/**
 * The conversation list — used twice, as the Chats tab and the Groups tab
 * (docs/ux/client-redesign.md section 5.1), filtered by [ConversationKind].
 *
 * Receiving is automatic (the app-level long poll in `MainActivity`), so there
 * is no Refresh button; pull-to-refresh runs the same poll once. Row actions
 * (pin / mute / archive) live behind a long-press menu, never as permanent
 * buttons in the row. The full row redesign is Phase B.
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
    val directory = remember(repository) { me.lakko.app.profile.ProfileDirectory.get(repository.context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val flagsState = deviceGroupSync?.flags?.collectAsStateWithLifecycle()
    val flagsMap: Map<String, FlagsState> = flagsState?.value ?: emptyMap()
    val readCursorState = deviceGroupSync?.readCursors?.collectAsStateWithLifecycle()
    val readCursorMap: Map<String, ReadCursorState> =
        readCursorState?.value ?: emptyMap()
    var refreshing by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) {
        // One immediate poll so the list is fresh on arrival; the continuous
        // long poll across every group runs app-wide from `MainActivity`.
        repository.pollAllGroups()
    }

    val isChats = kind == ConversationKind.Chat
    val visible = remember(state, kind, nameVersion) {
        state.threads.filter { conversationKind(context, repository, it.groupIdHex) == kind }
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
                if (visible.isEmpty()) {
                    EmptyPanel(isChats = isChats, onCompose = onCompose)
                } else {
                    val sorted = visible.sortedWith(
                        compareByDescending<ChatThread> { thread ->
                            flagsMap[thread.groupIdHex]?.pinned == true
                        }.thenBy { thread ->
                            flagsMap[thread.groupIdHex]?.archived == true
                        },
                    )
                    LazyColumn(
                        modifier = Modifier.fillMaxSize(),
                        contentPadding = PaddingValues(vertical = 4.dp),
                    ) {
                        items(items = sorted, key = { it.groupIdHex }) { thread ->
                            val unread = unreadCount(
                                thread = thread,
                                cursor = readCursorMap[thread.groupIdHex],
                            )
                            val title = remember(thread.groupIdHex, nameVersion, state) {
                                conversationTitle(context, repository, directory, thread.groupIdHex)
                            }
                            ThreadRow(
                                thread = thread,
                                title = title,
                                isGroup = !isChats,
                                flags = flagsMap[thread.groupIdHex],
                                unread = unread,
                                onClick = { onOpenThread(thread.groupIdHex) },
                                onToggleFlag = { field ->
                                    val sync = deviceGroupSync ?: return@ThreadRow
                                    val groupId = hexToBytes(thread.groupIdHex)
                                        ?: return@ThreadRow
                                    val cur = flagsMap[thread.groupIdHex]
                                    var pinned = cur?.pinned ?: false
                                    var muted = cur?.muted ?: false
                                    var archived = cur?.archived ?: false
                                    val unreadOverride =
                                        cur?.unreadOverride ?: false
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
                                            android.util.Log.w(
                                                "ChatListScreen",
                                                "setFlags failed",
                                                cause,
                                            )
                                        }
                                    }
                                },
                            )
                        }
                    }
                }
            }
        }
    }
}

internal enum class FlagField { Pin, Mute, Archive }

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

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ThreadRow(
    thread: ChatThread,
    title: String,
    isGroup: Boolean,
    flags: FlagsState?,
    unread: Int,
    onClick: () -> Unit,
    onToggleFlag: (FlagField) -> Unit,
) {
    val lastMessage = thread.messages.lastOrNull()
    val archived = flags?.archived == true
    val pinned = flags?.pinned == true
    val muted = flags?.muted == true
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
            LakkoAvatar(name = title, seed = thread.groupIdHex, size = 40.dp, group = isGroup)
            Spacer(Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = title,
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = if (unread > 0 && !thread.membershipEnded) FontWeight.SemiBold else FontWeight.Medium,
                        maxLines = 1,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                    if (muted) {
                        Spacer(Modifier.width(4.dp))
                        Icon(
                            Icons.Filled.NotificationsOff,
                            contentDescription = "Muted",
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                    if (pinned) {
                        Spacer(Modifier.width(4.dp))
                        Icon(
                            Icons.Filled.PushPin,
                            contentDescription = "Pinned",
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                    if (archived) {
                        Spacer(Modifier.width(4.dp))
                        Icon(
                            Icons.Filled.Archive,
                            contentDescription = "Archived",
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.size(14.dp),
                        )
                    }
                }
                Spacer(Modifier.height(2.dp))
                // ADR-0045 UX §6.2.4 — the chat STAYS in the list and gains one
                // muted line in the preview slot once this device's membership
                // has ended; flags are deliberately untouched.
                val endedLine = me.lakko.app.chat.chatListLine(
                    me.lakko.app.chat.membershipState(
                        isActive = !thread.membershipEnded,
                        leaveRequestedAtMs = null,
                    ),
                )
                val preview = when {
                    endedLine != null -> endedLine
                    lastMessage == null -> "No messages yet"
                    else -> {
                        val prefix = when (lastMessage.direction) {
                            Direction.Outgoing -> "You: "
                            Direction.Incoming -> ""
                            Direction.System -> ""
                        }
                        "$prefix${lastMessage.text}"
                    }
                }
                Text(
                    text = preview,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
            // ADR-0045 UX §6.2.4 / §6.2.11 — no badge once the membership has
            // ended; the count would be frozen for good.
            if (unread > 0 && !thread.membershipEnded) {
                Spacer(Modifier.width(8.dp))
                val label = if (unread > 99) "99+" else unread.toString()
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
                leadingIcon = { Icon(Icons.Filled.Archive, contentDescription = null) },
                onClick = { menuOpen = false; onToggleFlag(FlagField.Archive) },
            )
        }
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
