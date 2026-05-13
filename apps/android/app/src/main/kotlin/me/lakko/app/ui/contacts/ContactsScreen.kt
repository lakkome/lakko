// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import delay
import isActive
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.IncomingContactRequest
import me.lakko.app.contacts.SentContactRequest

/**
 * Compose Contacts screen — search + send + pending / accepted /
 * sent lists. Minimal scope for 1.15e6:
 *
 *   * Top: back button + screen title + manual refresh button.
 *   * Send section: username textfield + intro textfield + Send
 *     button. Errors render inline below the form.
 *   * Pending incoming requests: each row shows the sender's
 *     account-id hex + intro text (if decrypted) + Accept /
 *     Decline buttons.
 *   * Sent log: each row shows the recipient username + message
 *     id. No undo (the server doesn't expose a cancel endpoint
 *     yet; the recipient simply sees the request in their
 *     stranger inbox).
 *   * Accepted list: just shows the row was accepted —
 *     no chat yet, that's 1.15f.
 *   * "Re-enter password" panel renders when the auth session is
 *     restored from disk without an in-memory export key.
 *
 * No continuous polling — refresh is manual. Once the user
 * navigates into Contacts we kick off one auto-refresh via
 * [LaunchedEffect], then they refresh by tapping the button.
 */
@Composable
public fun ContactsScreen(
    viewModel: ContactsViewModel,
    onNavigateBack: () -> Unit,
    /** Called when the user taps an accepted contact's "Open
     *  chat" affordance. The parent route navigates to the 1×1
     *  ChatScreen for the given group. `null` when 1.15f4c's
     *  chat-bridge wiring isn't active (tests). */
    onOpenChat: ((groupIdHex: String) -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    // Auto-refresh on first arrival + continuous long-poll
    // while the screen is visible. The `LaunchedEffect(Unit)`
    // coroutine cancels automatically when the composable
    // leaves composition (back navigation, logout, etc.).
    //
    // The first iteration uses the manual-refresh path
    // (`onRefreshClicked`) so the spinner blips on / off; the
    // subsequent loop uses `pollOnceLongPoll(waitSecs = 30)`
    // which is silent (no spinner flicker for 30s at a time).
    LaunchedEffect(Unit) {
        viewModel.onRefreshClicked()
        while (isActive) {
            try {
                viewModel.pollOnceLongPoll()
            } catch (_: Throwable) {
                // Back off on transport / server errors so we
                // don't tight-loop against a 503. The
                // composable still gets the next user-driven
                // refresh via the header button.
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
            refreshing = state.refreshing,
            onRefresh = viewModel::onRefreshClicked,
        )
        Spacer(Modifier.height(8.dp))

        if (state.requiresReauth) {
            ReauthPanel()
            return@Column
        }

        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
        ) {
            SendSection(
                usernameInput = state.usernameInput,
                introInput = state.introInput,
                sending = state.sending,
                error = state.sendError,
                onUsernameChange = viewModel::onUsernameChange,
                onIntroChange = viewModel::onIntroChange,
                onSendClicked = viewModel::onSendClicked,
            )

            if (state.refreshError != null) {
                Spacer(Modifier.height(8.dp))
                Text(
                    text = "Inbox: ${state.refreshError}",
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            Spacer(Modifier.height(24.dp))
            PendingSection(
                pending = state.pending,
                onAccept = viewModel::onAcceptClicked,
                onDecline = viewModel::onDeclineClicked,
            )

            Spacer(Modifier.height(24.dp))
            AcceptedSection(accepted = state.accepted, onOpenChat = onOpenChat)

            Spacer(Modifier.height(24.dp))
            SentSection(sent = state.sent)

            Spacer(Modifier.height(32.dp))
        }
    }
}

// =============================================================================
// Header
// =============================================================================

@Composable
private fun Header(
    onNavigateBack: () -> Unit,
    refreshing: Boolean,
    onRefresh: () -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        TextButton(onClick = onNavigateBack) {
            Text("← Back")
        }
        Spacer(Modifier.width(8.dp))
        Text(
            text = "Contacts",
            style = MaterialTheme.typography.titleLarge,
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
            TextButton(onClick = onRefresh) {
                Text("Refresh")
            }
        }
    }
}

// =============================================================================
// Re-auth panel
// =============================================================================

@Composable
private fun ReauthPanel() {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(top = 32.dp),
        verticalArrangement = Arrangement.Top,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = "Please re-enter your password",
            style = MaterialTheme.typography.titleMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            text = "Contacts require your in-memory identity key. Log out and " +
                "log back in to refresh it.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

// =============================================================================
// Send section
// =============================================================================

@Composable
private fun SendSection(
    usernameInput: String,
    introInput: String,
    sending: Boolean,
    error: String?,
    onUsernameChange: (String) -> Unit,
    onIntroChange: (String) -> Unit,
    onSendClicked: () -> Unit,
) {
    Text(
        text = "Send contact request",
        style = MaterialTheme.typography.titleMedium,
    )
    Spacer(Modifier.height(8.dp))

    OutlinedTextField(
        value = usernameInput,
        onValueChange = onUsernameChange,
        label = { Text("Username (e.g. alice)") },
        singleLine = true,
        enabled = !sending,
        keyboardOptions = KeyboardOptions(
            capitalization = KeyboardCapitalization.None,
            autoCorrect = false,
            keyboardType = KeyboardType.Text,
        ),
        modifier = Modifier.fillMaxWidth(),
    )
    Spacer(Modifier.height(8.dp))

    OutlinedTextField(
        value = introInput,
        onValueChange = onIntroChange,
        label = { Text("Intro message (optional)") },
        singleLine = false,
        enabled = !sending,
        modifier = Modifier.fillMaxWidth(),
    )

    if (error != null) {
        Spacer(Modifier.height(8.dp))
        Text(
            text = error,
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodyMedium,
        )
    }

    Spacer(Modifier.height(12.dp))
    Button(
        onClick = onSendClicked,
        enabled = !sending,
        modifier = Modifier.fillMaxWidth(),
    ) {
        if (sending) {
            CircularProgressIndicator(
                modifier = Modifier
                    .height(20.dp)
                    .width(20.dp),
                strokeWidth = 2.dp,
                color = MaterialTheme.colorScheme.onPrimary,
            )
            Spacer(Modifier.width(8.dp))
            Text("Sending…")
        } else {
            Text("Send request")
        }
    }
}

// =============================================================================
// Pending / accepted / sent sections
// =============================================================================

@Composable
private fun PendingSection(
    pending: List<IncomingContactRequest>,
    onAccept: (Long) -> Unit,
    onDecline: (Long) -> Unit,
) {
    SectionHeader(title = "Pending", count = pending.size)
    if (pending.isEmpty()) {
        EmptyHint(text = "No incoming requests.")
        return
    }
    Spacer(Modifier.height(8.dp))
    // Plain Column instead of LazyColumn because the parent
    // already scrolls vertically; nesting a LazyColumn inside a
    // vertical-scroll Column would Compose-warning + duplicate
    // the gesture detector.
    for (req in pending) {
        PendingRow(req = req, onAccept = onAccept, onDecline = onDecline)
        Spacer(Modifier.height(8.dp))
    }
}

@Composable
private fun PendingRow(
    req: IncomingContactRequest,
    onAccept: (Long) -> Unit,
    onDecline: (Long) -> Unit,
) {
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant,
        ),
        shape = RoundedCornerShape(8.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(
                text = "From pubkey ${shortHex(req.fromPubkey)}",
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
            )
            if (req.intro != null && req.intro.isNotBlank()) {
                Spacer(Modifier.height(4.dp))
                Text(
                    text = req.intro,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onAccept(req.messageId) },
                    modifier = Modifier.weight(1f),
                ) { Text("Accept") }
                OutlinedButton(
                    onClick = { onDecline(req.messageId) },
                    modifier = Modifier.weight(1f),
                ) { Text("Decline") }
            }
        }
    }
}

@Composable
private fun AcceptedSection(
    accepted: List<AcceptedContact>,
    onOpenChat: ((groupIdHex: String) -> Unit)?,
) {
    SectionHeader(title = "Accepted", count = accepted.size)
    if (accepted.isEmpty()) {
        EmptyHint(text = "No accepted contacts.")
        return
    }
    Spacer(Modifier.height(8.dp))
    for (c in accepted) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 4.dp),
        ) {
            Text(
                text = shortHex(c.request.fromPubkey),
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
                modifier = Modifier.weight(1f),
            )
            // "Open chat" affordance — shown only when the
            // 1.15f4c bridge wired the chat handshake at
            // Accept time. `null` for legacy v1 envelopes (no
            // sender account-id) or for accepts that happened
            // while ChatRepository was unbootstrapped; the row
            // surfaces a tiny "no chat" hint instead.
            if (c.groupIdHex != null && onOpenChat != null) {
                val groupIdHex = c.groupIdHex
                Button(
                    onClick = { onOpenChat(groupIdHex) },
                ) {
                    Text("Open chat")
                }
            } else {
                Text(
                    text = "no chat",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun SentSection(sent: List<SentContactRequest>) {
    SectionHeader(title = "Sent (this session)", count = sent.size)
    if (sent.isEmpty()) {
        EmptyHint(text = "No outgoing requests yet.")
        return
    }
    Spacer(Modifier.height(8.dp))
    for (s in sent) {
        Row(
            horizontalArrangement = Arrangement.SpaceBetween,
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 2.dp),
        ) {
            Text(
                text = "@${s.recipientUsername}",
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                text = "#${s.messageId}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontFamily = FontFamily.Monospace,
            )
        }
    }
}

@Composable
private fun SectionHeader(title: String, count: Int) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(
            text = title,
            style = MaterialTheme.typography.titleMedium,
        )
        Spacer(Modifier.width(4.dp))
        Text(
            text = "($count)",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    Spacer(Modifier.height(4.dp))
    HorizontalDivider()
}

@Composable
private fun EmptyHint(text: String) {
    Spacer(Modifier.height(4.dp))
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp),
    ) {
        Text(
            text = text,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** Back-off delay between failed long-poll iterations. 5s
 *  matches the canonical chat-app retry cadence — long enough
 *  to absorb a 503 burst, short enough that a transient blip
 *  doesn't degrade UX noticeably. */
private const val POLL_RETRY_BACKOFF_MS: Long = 5_000L

/** Short hex preview of a 32-byte public key — first 6 + last 4
 *  hex chars, separated by an ellipsis. Useful as a row label
 *  while the username-resolution layer doesn't carry handles
 *  back from the inbox side. */
private fun shortHex(bytes: ByteArray): String {
    val full = StringBuilder(bytes.size * 2)
    for (b in bytes) {
        val v = b.toInt() and 0xFF
        full.append("0123456789abcdef"[v ushr 4])
        full.append("0123456789abcdef"[v and 0x0F])
    }
    if (full.length <= 12) return full.toString()
    return "${full.substring(0, 6)}…${full.substring(full.length - 4)}"
}
