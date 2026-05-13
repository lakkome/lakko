// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

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
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import delay
import isActive
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
) {
    val state by repository.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()

    LaunchedEffect(Unit) {
        // First tick: immediate refresh so the list is fresh
        // when the user lands. Subsequent ticks: continuous
        // long-poll across every active group. Cancels when
        // the composable leaves composition.
        scope.launch { repository.pollAllGroups() }
        while (isActive) {
            try {
                repository.pollAllGroups(waitSecs = LONG_POLL_SECS)
            } catch (_: Throwable) {
                delay(POLL_RETRY_BACKOFF_MS)
            }
        }
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
        Spacer(Modifier.height(8.dp))

        if (state.threads.isEmpty()) {
            EmptyPanel()
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(items = state.threads, key = { it.groupIdHex }) { thread ->
                    ThreadRow(
                        thread = thread,
                        onClick = { onOpenThread(thread.groupIdHex) },
                    )
                }
            }
        }
    }
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
    onClick: () -> Unit,
) {
    val lastMessage = thread.messages.lastOrNull()
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant,
        ),
        shape = RoundedCornerShape(8.dp),
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(
                text = "Group ${shortHex(thread.groupIdHex)}",
                style = MaterialTheme.typography.titleSmall,
                fontFamily = FontFamily.Monospace,
            )
            Spacer(Modifier.height(4.dp))
            if (lastMessage == null) {
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

/** Long-poll constants — same values as the per-thread
 *  ChatScreen + the ContactsScreen polling loops. */
private const val LONG_POLL_SECS: Int = 30
private const val POLL_RETRY_BACKOFF_MS: Long = 5_000L

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
