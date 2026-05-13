// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

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
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction

/**
 * 1×1 chat screen — `LazyColumn` of messages + composer.
 *
 * Mirror of `apps/web`'s eventual ChatRoute. Minimal Material3:
 * outgoing rows right-aligned in a primary-tinted bubble,
 * incoming rows left-aligned in a surface-variant bubble,
 * system rows centered + smaller font. The auto-scroll-to-
 * bottom on new messages (the canonical chat-UX touch) is
 * one `LaunchedEffect(messages.size)` away.
 *
 * No continuous polling — `LaunchedEffect(Unit)` fires one
 * refresh on first arrival; the user taps Refresh in the
 * header thereafter. Same posture as 1.15e6's
 * [me.lakko.app.ui.contacts.ContactsScreen] +
 * 1.15f4b's [ChatListScreen].
 *
 * Composer uses the standard `OutlinedTextField` with
 * multi-line support (`singleLine = false`) — paste a 5-line
 * blob and it renders as five lines, send wraps everything
 * into one Application-message. Send button is disabled when
 * the draft is blank or a send is in flight.
 */
@Composable
public fun ChatScreen(
    viewModel: ChatViewModel,
    onNavigateBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val listState = rememberLazyListState()

    LaunchedEffect(Unit) {
        viewModel.onRefreshClicked()
    }
    LaunchedEffect(state.messages.size) {
        // Auto-scroll to the latest row on every new message
        // (Outgoing send + Incoming poll both bump the count).
        if (state.messages.isNotEmpty()) {
            listState.animateScrollToItem(state.messages.size - 1)
        }
    }

    Column(modifier = modifier.fillMaxSize()) {
        Header(
            groupIdHex = state.groupIdHex,
            refreshing = state.refreshing,
            onNavigateBack = onNavigateBack,
            onRefresh = viewModel::onRefreshClicked,
        )

        if (!state.threadKnown) {
            // The chat list opened this screen for a group that
            // the repository doesn't know about yet (post-restart
            // before snapshot rehydrate built the handle, or a
            // bug in the route argument). Surface the missing
            // state without crashing — the composer below still
            // works for the user to type but sendMessage will
            // surface a clear error.
            ThreadMissingHint()
        }

        // Message list takes the available height; composer
        // pinned to the bottom.
        Box(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .padding(horizontal = 12.dp),
        ) {
            if (state.messages.isEmpty()) {
                EmptyTimelineHint()
            } else {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(
                        vertical = 8.dp,
                    ),
                ) {
                    items(items = state.messages, key = { it.messageId }) { msg ->
                        MessageRow(msg)
                    }
                }
            }
        }

        Composer(
            draft = state.draft,
            sending = state.sending,
            sendError = state.sendError,
            onDraftChange = viewModel::onDraftChange,
            onSendClicked = viewModel::onSendClicked,
        )
    }
}

// =============================================================================
// Header
// =============================================================================

@Composable
private fun Header(
    groupIdHex: String,
    refreshing: Boolean,
    onNavigateBack: () -> Unit,
    onRefresh: () -> Unit,
) {
    Column {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = 4.dp, end = 12.dp, top = 4.dp, bottom = 4.dp),
        ) {
            TextButton(onClick = onNavigateBack) { Text("← Back") }
            Spacer(Modifier.width(4.dp))
            Text(
                text = "Group ${shortHex(groupIdHex)}",
                style = MaterialTheme.typography.titleMedium,
                fontFamily = FontFamily.Monospace,
                modifier = Modifier.weight(1f),
            )
            if (refreshing) {
                CircularProgressIndicator(
                    modifier = Modifier
                        .height(20.dp)
                        .width(20.dp),
                    strokeWidth = 2.dp,
                )
            } else {
                TextButton(onClick = onRefresh) { Text("Refresh") }
            }
        }
        HorizontalDivider()
    }
}

// =============================================================================
// Timeline rows
// =============================================================================

@Composable
private fun MessageRow(msg: ChatMessage) {
    when (msg.direction) {
        Direction.Outgoing -> Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.End,
        ) {
            Bubble(
                text = msg.text,
                background = MaterialTheme.colorScheme.primaryContainer,
                contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
            )
        }

        Direction.Incoming -> Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.Start,
        ) {
            Bubble(
                text = msg.text,
                background = MaterialTheme.colorScheme.surfaceVariant,
                contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        Direction.System -> Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.Center,
        ) {
            Text(
                text = msg.text,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontFamily = FontFamily.Monospace,
                modifier = Modifier.padding(vertical = 4.dp),
            )
        }
    }
}

@Composable
private fun Bubble(
    text: String,
    background: androidx.compose.ui.graphics.Color,
    contentColor: androidx.compose.ui.graphics.Color,
) {
    Surface(
        shape = RoundedCornerShape(12.dp),
        color = background,
        contentColor = contentColor,
        modifier = Modifier.widthIn(max = 280.dp),
    ) {
        Text(
            text = text,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
        )
    }
}

@Composable
private fun EmptyTimelineHint() {
    Box(
        modifier = Modifier.fillMaxSize(),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = "No messages yet. Say hi.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ThreadMissingHint() {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 8.dp),
    ) {
        Text(
            text = "This conversation hasn't been hydrated on this device yet. " +
                "Tap Refresh — a peer's commit will rebuild it.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

// =============================================================================
// Composer
// =============================================================================

@Composable
private fun Composer(
    draft: String,
    sending: Boolean,
    sendError: String?,
    onDraftChange: (String) -> Unit,
    onSendClicked: () -> Unit,
) {
    Column {
        HorizontalDivider()
        if (sendError != null) {
            Text(
                text = sendError,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
            )
        }
        Row(
            verticalAlignment = Alignment.Bottom,
            modifier = Modifier
                .fillMaxWidth()
                .padding(12.dp),
        ) {
            OutlinedTextField(
                value = draft,
                onValueChange = onDraftChange,
                placeholder = { Text("Type a message…") },
                singleLine = false,
                enabled = !sending,
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.Sentences,
                ),
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(8.dp))
            Button(
                onClick = onSendClicked,
                enabled = !sending && draft.isNotBlank(),
            ) {
                if (sending) {
                    CircularProgressIndicator(
                        modifier = Modifier
                            .height(20.dp)
                            .width(20.dp),
                        strokeWidth = 2.dp,
                        color = MaterialTheme.colorScheme.onPrimary,
                    )
                } else {
                    Text("Send")
                }
            }
        }
    }
}

/** Short hex preview — same format as
 *  [me.lakko.app.ui.contacts.ContactsScreen]'s `shortHex`. */
private fun shortHex(hex: String): String {
    if (hex.length <= 12) return hex
    return "${hex.substring(0, 6)}…${hex.substring(hex.length - 4)}"
}
