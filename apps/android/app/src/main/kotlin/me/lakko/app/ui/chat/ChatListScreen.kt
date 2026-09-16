// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.devices.sync.FlagsState
import me.lakko.app.devices.sync.ReadCursorState
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ChatThread
import me.lakko.app.chat.Direction

/**
 * Chat list screen — vertical list of every conversation
 * thread `ChatRepository` knows about. Mirror of `apps/web`'s
 * (eventual) ChatList route; for 1.15f4b the surface is
 * minimal:
 *
 *   * Top: back button + title + manual refresh (`pollAllGroups`).
 *   * Body: `LazyColumn` of [ChatThread]s as Cards. Each card
 *     shows the short hex of the group id + the last message
 *     preview + tap → [onOpenThread].
 *   * Empty state: gentle hint that contacts → Accept will
 *     create a thread.
 *
 * No continuous polling — `LaunchedEffect(Unit)` fires one
 * refresh on first arrival, then the user taps Refresh. Same
 * posture as 1.15e6's [me.lakko.app.ui.contacts.ContactsScreen].
 *
 * The screen consumes the repository directly rather than via a
 * dedicated ViewModel because the surface is purely a read-only
 * projection — no composer, no per-thread mutable state. The
 * 1×1 [ChatScreen] does have a ChatViewModel for its composer
 * + send-busy flags.
 */
@Composable
public fun ChatListScreen(
    repository: ChatRepository,
    onNavigateBack: () -> Unit,
    onOpenThread: (groupIdHex: String) -> Unit,
    modifier: Modifier = Modifier,
    deviceGroupSync: DeviceGroupSyncRepository? = null,
) {
    val state by repository.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    // ADR-0048 UX §5 — a two-member chat is listed under the other person's
    // resolved name (`Name · @username` when verified); a group keeps its label.
    val directory = remember(repository) { me.lakko.app.profile.ProfileDirectory.get(repository.context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    // Phase 2a3e2c-Android — cross-device flags. Nullable
    // `deviceGroupSync` keeps the screen mountable in tests /
    // older callers that haven't wired the sync runtime yet.
    val flagsState = deviceGroupSync?.flags?.collectAsStateWithLifecycle()
    val flagsMap: Map<String, FlagsState> = flagsState?.value ?: emptyMap()
    // Phase 2a3e2c-followup — per-thread read cursor map for
    // the unread-badge counter.
    val readCursorState = deviceGroupSync?.readCursors?.collectAsStateWithLifecycle()
    val readCursorMap: Map<String, ReadCursorState> =
        readCursorState?.value ?: emptyMap()

    LaunchedEffect(Unit) {
        // An immediate refresh so the list is fresh when the user lands. The
        // continuous long poll across every group is NOT here any more: it runs
        // app-wide from `MainActivity`'s session root while the app is in the
        // foreground, whatever screen is showing, because call signaling and
        // the unread counts must arrive for chats that are not open on screens
        // that are not this one. A second loop here would only lose the
        // per-group in-flight race to it.
        repository.pollAllGroups()
    }

    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(16.dp),
    ) {
        Header(
            onNavigateBack = onNavigateBack,
            onRefresh = { scope.launch { repository.pollAllGroups() } },
        )
        // ADR-0050 / docs/ux/second-device.md section 2: one label while this
        // device is still being admitted to the account's chats. Chats appear
        // as they arrive; the label goes when nothing is pending. Never a
        // counter, never a per-chat row. Re-read on every repository emission.
        val synchronizing = remember(state) { repository.isSynchronizing() }
        if (synchronizing) {
            Text(
                "Synchronizing...",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Spacer(Modifier.height(8.dp))

        if (state.threads.isEmpty()) {
            EmptyPanel()
        } else {
            // Phase 2a3e2c — pinned threads to the top,
            // archived to the bottom, otherwise stable on the
            // repository order.
            val sorted = state.threads.sortedWith(
                compareByDescending<ChatThread> { thread ->
                    flagsMap[thread.groupIdHex]?.pinned == true
                }.thenBy { thread ->
                    flagsMap[thread.groupIdHex]?.archived == true
                },
            )
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(items = sorted, key = { it.groupIdHex }) { thread ->
                    val unread = unreadCount(
                        thread = thread,
                        cursor = readCursorMap[thread.groupIdHex],
                    )
                    val peerKey = repository.oneToOnePeerKey(thread.groupIdHex)
                    ThreadRow(
                        thread = thread,
                        title = nameVersion.let { peerKey?.let { directory.resolve(it).listLabel } },
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

@Composable
private fun Header(
    onNavigateBack: () -> Unit,
    onRefresh: () -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        TextButton(onClick = onNavigateBack) { Text("← Back") }
        Spacer(Modifier.width(8.dp))
        Text(
            text = "Chats",
            style = MaterialTheme.typography.titleLarge,
            modifier = Modifier.weight(1f),
        )
        TextButton(onClick = onRefresh) { Text("Refresh") }
    }
    HorizontalDivider()
}

@Composable
private fun ThreadRow(
    thread: ChatThread,
    /** ADR-0048 — the other person's resolved name for a 1x1 chat, else `null`. */
    title: String?,
    flags: FlagsState?,
    unread: Int,
    onClick: () -> Unit,
    onToggleFlag: (FlagField) -> Unit,
) {
    val lastMessage = thread.messages.lastOrNull()
    val archived = flags?.archived == true
    val pinned = flags?.pinned == true
    val muted = flags?.muted == true
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant,
        ),
        shape = RoundedCornerShape(8.dp),
        modifier = Modifier
            .fillMaxWidth()
            .alpha(if (archived) 0.6f else 1f)
            .clickable(onClick = onClick),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (pinned) {
                            Text("📌 ", style = MaterialTheme.typography.titleSmall)
                        }
                        Text(
                            text = title ?: "Group ${shortHex(thread.groupIdHex)}",
                            style = MaterialTheme.typography.titleSmall,
                            fontFamily = if (title == null) FontFamily.Monospace else FontFamily.Default,
                            maxLines = 1,
                        )
                        if (muted) {
                            Text(
                                " 🔇",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        if (archived) {
                            Text(
                                " 📦",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
                // Phase 2a3e2c-followup — unread badge.
                // Muted rows render a hollow ring; everyone
                // else gets the accent fill.
                //
                // ADR-0045 UX §6.2.4 / §6.2.11 — the row goes QUIET once this
                // device's membership has ended. An unread badge asserts a
                // conversation that can still move, and this one cannot:
                // nothing new will ever decrypt, so whatever count is standing
                // is frozen at the moment of the eviction and would sit there
                // for good.
                if (unread > 0 && !thread.membershipEnded) {
                    val label = if (unread > 99) "99+" else unread.toString()
                    androidx.compose.foundation.layout.Box(
                        contentAlignment = Alignment.Center,
                        modifier = Modifier
                            .padding(end = 4.dp)
                            .clip(RoundedCornerShape(percent = 50))
                            .background(
                                if (muted) MaterialTheme.colorScheme.surface
                                else MaterialTheme.colorScheme.primary,
                            )
                            .padding(horizontal = 6.dp, vertical = 2.dp),
                    ) {
                        Text(
                            text = label,
                            style = MaterialTheme.typography.labelSmall,
                            color = if (muted) MaterialTheme.colorScheme.onSurfaceVariant
                            else MaterialTheme.colorScheme.onPrimary,
                        )
                    }
                }
                IconButton(onClick = { onToggleFlag(FlagField.Pin) }) {
                    Text(
                        if (pinned) "📌" else "📍",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
                IconButton(onClick = { onToggleFlag(FlagField.Mute) }) {
                    Text(
                        if (muted) "🔇" else "🔈",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
                IconButton(onClick = { onToggleFlag(FlagField.Archive) }) {
                    Text(
                        if (archived) "📤" else "📦",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
            Spacer(Modifier.height(4.dp))
            // ADR-0045 UX §6.2.4 — the chat STAYS in the list and gains one
            // muted line, in the same slot the last-message preview already
            // uses.
            //
            // It stays because removing it would delete a transcript the user
            // still has, with no warning, as a side effect of something
            // somebody else did — the single worst thing in this design's
            // reach. The row keeps its position, its label and its flags:
            // `pinned` / `muted` / `archived` are deliberately untouched,
            // because auto-archiving would be the client silently reorganising
            // the user's list on their behalf, and archive is a decision, not a
            // consequence.
            //
            // The line replaces the preview rather than sitting above it: the
            // preview is the "what is happening here" slot, and the honest
            // answer is now this. The transcript itself is one tap away and
            // fully intact.
            val endedLine = me.lakko.app.chat.chatListLine(
                me.lakko.app.chat.membershipState(
                    // Already folded by the repository, where the live handle
                    // is: a torn read there answers "still a member".
                    isActive = !thread.membershipEnded,
                    // Not consulted — `chatListLine` returns the same string
                    // either way. The left / removed distinction belongs in the
                    // transcript's terminal row, where the durable record is
                    // read; the list is a glance surface.
                    leaveRequestedAtMs = null,
                ),
            )
            if (endedLine != null) {
                Text(
                    text = endedLine,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else if (lastMessage == null) {
                Text(
                    text = "No messages yet.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                val prefix = when (lastMessage.direction) {
                    Direction.Outgoing -> "you: "
                    Direction.Incoming -> ""
                    Direction.System -> ""
                }
                Text(
                    text = "$prefix${lastMessage.text}",
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 2,
                )
            }
            Spacer(Modifier.height(4.dp))
            Text(
                text = "${thread.messages.size} message" +
                    if (thread.messages.size == 1) "" else "s",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun EmptyPanel() {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(top = 32.dp),
        contentAlignment = Alignment.TopCenter,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                text = "No chats yet",
                style = MaterialTheme.typography.titleMedium,
            )
            Spacer(Modifier.height(4.dp))
            Text(
                text = "Open Contacts and Accept an incoming request to start " +
                    "a 1×1 chat. Outgoing requests turn into chats when the " +
                    "recipient accepts (the next time their app polls the " +
                    "stranger inbox).",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}


/** Show the first six + last four hex chars of a long string —
 *  same preview format as
 *  [me.lakko.app.ui.contacts.ContactsScreen]'s `shortHex`. */
private fun shortHex(hex: String): String {
    if (hex.length <= 12) return hex
    return "${hex.substring(0, 6)}…${hex.substring(hex.length - 4)}"
}

/** Sentinel for an empty refreshing spinner — unused today but
 *  kept here so 1.15f4c's per-thread refresh indicator has a
 *  consistent style entry point. */
@Suppress("unused")
@Composable
private fun TinySpinner() {
    CircularProgressIndicator(
        modifier = Modifier
            .height(14.dp)
            .width(14.dp),
        strokeWidth = 2.dp,
    )
}
