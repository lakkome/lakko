// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * Invite-landing screen — joiner-side terminal display + the
 * `Open chat` affordance when the MLS handshake succeeded.
 *
 * The screen does not gate the Accept dance behind a user
 * gesture: [InviteLandingViewModel] auto-runs
 * `acceptInviteLink(slug)` on `init`, so by the time the user
 * sees the screen the orchestration is already in flight.
 * That matches the standard "tap a link, the app opens, the
 * accept happens" UX users expect from messenger invite links.
 *
 * Each of the eight [InviteLandingState] variants gets a
 * minimal Material3 panel:
 *
 *   * [Loading] — spinner + "Accepting invite…"
 *   * [Accepted] with `groupIdHex != null` — confirmation +
 *     `Open chat` primary + `Done` outlined
 *   * [Accepted] with `groupIdHex == null` — same confirmation
 *     copy, MLS-handshake hint, `Done` only (the chat surface
 *     wasn't wired or the handshake failed; the contact is
 *     still in the accepted list)
 *   * [AlreadyAccepted] / [Expired] / [Exhausted] / [NotFound]
 *     / [MalformedSlug] — terminal informational panels +
 *     `Done`
 *   * [RequiresReauth] — "log in first" hint + `Done`
 *   * [Failed] — error message + `Retry` primary + `Done`
 *     outlined
 */
@Composable
public fun InviteLandingScreen(
    viewModel: InviteLandingViewModel,
    onDone: () -> Unit,
    onOpenChat: (groupIdHex: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = "Lakko invite",
            style = MaterialTheme.typography.titleLarge,
        )
        Spacer(Modifier.height(16.dp))

        when (val s = state) {
            InviteLandingState.Loading -> LoadingPanel()
            is InviteLandingState.Accepted -> AcceptedPanel(
                state = s,
                onOpenChat = onOpenChat,
                onDone = onDone,
            )
            InviteLandingState.AlreadyAccepted -> InfoPanel(
                title = "Already accepted",
                body = "You've already accepted this invite. The contact is in " +
                    "your Accepted list — Open Contacts to find them.",
                onDone = onDone,
            )
            InviteLandingState.Expired -> InfoPanel(
                title = "Invite expired",
                body = "This invite link is past its expiry. Ask your friend " +
                    "to share a fresh one.",
                onDone = onDone,
            )
            InviteLandingState.Exhausted -> InfoPanel(
                title = "Invite used up",
                body = "This invite reached its consume limit. Ask your friend " +
                    "to share a fresh one.",
                onDone = onDone,
            )
            InviteLandingState.NotFound -> InfoPanel(
                title = "Invite not found",
                body = "This invite has been revoked, has expired, or was never " +
                    "issued. Ask your friend to share a fresh one.",
                onDone = onDone,
            )
            InviteLandingState.MalformedSlug -> InfoPanel(
                title = "Bad invite URL",
                body = "The invite link looks corrupted. Ask your friend to " +
                    "share it again.",
                onDone = onDone,
            )
            InviteLandingState.RequiresReauth -> InfoPanel(
                title = "Log in to accept",
                body = "Your session needs a fresh password. Log out + log " +
                    "back in, then re-tap the invite link.",
                onDone = onDone,
            )
            is InviteLandingState.Failed -> FailedPanel(
                message = s.message,
                onRetry = viewModel::retry,
                onDone = onDone,
            )
        }
    }
}

@Composable
private fun LoadingPanel() {
    CircularProgressIndicator(
        modifier = Modifier
            .height(48.dp)
            .width(48.dp),
        strokeWidth = 4.dp,
    )
    Spacer(Modifier.height(16.dp))
    Text(
        text = "Accepting invite…",
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun AcceptedPanel(
    state: InviteLandingState.Accepted,
    onOpenChat: (String) -> Unit,
    onDone: () -> Unit,
) {
    Text(
        text = "Connected.",
        style = MaterialTheme.typography.titleMedium,
    )
    Spacer(Modifier.height(8.dp))
    Text(
        text = "Account id (base64): ${shortAccountId(state.recipientAccountId)}",
        style = MaterialTheme.typography.bodySmall,
        fontFamily = FontFamily.Monospace,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    if (state.groupIdHex == null) {
        Spacer(Modifier.height(8.dp))
        Text(
            text = "MLS chat session not built yet — Open Contacts to retry " +
                "the handshake.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    Spacer(Modifier.height(24.dp))
    Row(
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        if (state.groupIdHex != null) {
            val groupIdHex = state.groupIdHex
            Button(
                onClick = { onOpenChat(groupIdHex) },
                modifier = Modifier.weight(1f),
            ) { Text("Open chat") }
            OutlinedButton(
                onClick = onDone,
                modifier = Modifier.weight(1f),
            ) { Text("Done") }
        } else {
            Button(
                onClick = onDone,
                modifier = Modifier.weight(1f),
            ) { Text("Done") }
        }
    }
}

@Composable
private fun InfoPanel(
    title: String,
    body: String,
    onDone: () -> Unit,
) {
    Text(
        text = title,
        style = MaterialTheme.typography.titleMedium,
    )
    Spacer(Modifier.height(8.dp))
    Text(
        text = body,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(24.dp))
    Button(
        onClick = onDone,
        modifier = Modifier.fillMaxWidth(),
    ) { Text("Done") }
}

@Composable
private fun FailedPanel(
    message: String,
    onRetry: () -> Unit,
    onDone: () -> Unit,
) {
    Text(
        text = "Accept failed",
        style = MaterialTheme.typography.titleMedium,
        color = MaterialTheme.colorScheme.error,
    )
    Spacer(Modifier.height(8.dp))
    Text(
        text = message,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(24.dp))
    Row(
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Button(
            onClick = onRetry,
            modifier = Modifier.weight(1f),
        ) { Text("Retry") }
        OutlinedButton(
            onClick = onDone,
            modifier = Modifier.weight(1f),
        ) { Text("Done") }
    }
}

/** base64 the 16-byte UUIDv7, then truncate to a short
 *  human-readable preview (the full 22-char b64 is ugly and
 *  not actionable for the user — we just want to show "this
 *  is who you connected to" for confidence). */
private fun shortAccountId(bytes: ByteArray): String {
    val b64 = me.lakko.app.auth.base64Encode(bytes)
    return if (b64.length <= 12) b64 else "${b64.substring(0, 8)}…"
}
