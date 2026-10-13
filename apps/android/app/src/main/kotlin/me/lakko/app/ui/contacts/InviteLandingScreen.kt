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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text

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
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun InviteLandingScreen(
    viewModel: InviteLandingViewModel,
    onDone: () -> Unit,
    onOpenChat: (groupIdHex: String) -> Unit,
    /** ADR-0073 UX 16.6 — the link's owner is already a contact. */
    onOpenContact: (routeKey: String) -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("invite.contact.page_title")) },
                navigationIcon = {
                    IconButton(onClick = onDone) {
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
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {

        when (val s = state) {
            InviteLandingState.Loading -> LoadingPanel()
            is InviteLandingState.Accepted -> AcceptedPanel(
                state = s,
                onOpenChat = onOpenChat,
                onDone = onDone,
            )
            is InviteLandingState.ExistingContact -> {
                // Nothing to say: the existing contact opens.
                androidx.compose.runtime.LaunchedEffect(s.routeKey) { onOpenContact(s.routeKey) }
                LoadingPanel()
            }
            InviteLandingState.AlreadyAccepted -> InfoPanel(
                body = t("invite.contact.already_accepted_unnamed"),
                onDone = onDone,
            )
            InviteLandingState.Expired -> InfoPanel(
                body = t("invite.contact.expired"),
                onDone = onDone,
            )
            InviteLandingState.Exhausted -> InfoPanel(
                body = t("invite.contact.used_up"),
                onDone = onDone,
            )
            InviteLandingState.NotFound -> InfoPanel(
                body = t("invite.contact.no_longer_valid"),
                onDone = onDone,
            )
            InviteLandingState.MalformedSlug -> InfoPanel(
                title = t("invite.contact.invalid_title"),
                body = t("invite.link.damaged"),
                onDone = onDone,
            )
            InviteLandingState.Damaged -> DamagedPanel(onDone = onDone)
            InviteLandingState.RateLimited -> RateLimitedPanel(
                onRetry = viewModel::retry,
                onDone = onDone,
            )
            InviteLandingState.AllowanceUsed -> AllowanceUsedPanel(onDone = onDone)
            InviteLandingState.RequiresReauth -> InfoPanel(
                body = t("common.error.session_ended"),
                onDone = onDone,
            )
            // The failure's prose went to the log (ADR-0061 section 6).
            is InviteLandingState.Failed -> FailedPanel(
                onRetry = viewModel::retry,
                onDone = onDone,
            )
        }
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
        text = t("invite.contact.accepting"),
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
        text = t("invite.contact.connected"),
        style = MaterialTheme.typography.titleMedium,
    )
    if (state.groupIdHex == null) {
        Spacer(Modifier.height(8.dp))
        Text(
            text = t("contacts.error.start_chat"),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    Spacer(Modifier.height(16.dp))
    // Defence-in-depth anti-phishing surface — render the
    // Signal-style safety number derived from the inviter's
    // Ed25519 pubkey so the user can verify out-of-band that
    // the link wasn't substituted (ROADMAP 3c3 / calls.md
    // C-28). Mirror of the iOS + web equivalents.
    Surface(
        color = MaterialTheme.colorScheme.surfaceVariant,
        shape = RoundedCornerShape(10.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(12.dp),
        ) {
            Text(
                text = t("invite.contact.fingerprint"),
                style = MaterialTheme.typography.titleSmall,
            )
            Spacer(Modifier.height(4.dp))
            Text(
                text = state.inviterFingerprint,
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
            )
            Spacer(Modifier.height(6.dp))
            Text(
                text = t("invite.contact.check_body"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
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
            ) { Text(t("contacts.detail.message")) }
            OutlinedButton(
                onClick = onDone,
                modifier = Modifier.weight(1f),
            ) { Text(t("common.done")) }
        } else {
            Button(
                onClick = onDone,
                modifier = Modifier.weight(1f),
            ) { Text(t("common.done")) }
        }
    }
}

@Composable
private fun InfoPanel(
    body: String,
    onDone: () -> Unit,
    title: String? = null,
) {
    if (title != null) {
        Text(
            text = title,
            style = MaterialTheme.typography.titleMedium,
        )
        Spacer(Modifier.height(8.dp))
    }
    Text(
        text = body,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(24.dp))
    Button(
        onClick = onDone,
        modifier = Modifier.fillMaxWidth(),
    ) { Text(t("common.done")) }
}

/** Sealed links (`docs/ux/group-invite-page.md` section 3): the one
 *  sentence and Done, nothing else — nothing was opened. */
@Composable
private fun DamagedPanel(onDone: () -> Unit) {
    Text(
        text = me.lakko.app.discovery.INVITE_LINK_DAMAGED_COPY.text(),
        style = MaterialTheme.typography.bodyMedium,
    )
    Spacer(Modifier.height(24.dp))
    Button(
        onClick = onDone,
        modifier = Modifier.fillMaxWidth(),
    ) { Text(t("common.done")) }
}

/** ADR-0054 UX section 3 - the day's allowance of invite tokens is used up:
 *  the spec's sentence and Done, no retry (it would fail the same way until
 *  tomorrow). */
@Composable
private fun AllowanceUsedPanel(onDone: () -> Unit) {
    Text(
        text = t("tokens.action_allowance_used"),
        style = MaterialTheme.typography.bodyMedium,
    )
    Spacer(Modifier.height(24.dp))
    OutlinedButton(
        onClick = onDone,
        modifier = Modifier.fillMaxWidth(),
    ) { Text(t("common.done")) }
}

/** The discovery service answered `429`: honest copy and a retry, never
 *  "invalid link". */
@Composable
private fun RateLimitedPanel(
    onRetry: () -> Unit,
    onDone: () -> Unit,
) {
    Text(
        text = me.lakko.app.discovery.INVITE_RATE_LIMITED_TEXT.text(),
        style = MaterialTheme.typography.bodyMedium,
    )
    Spacer(Modifier.height(24.dp))
    Row(
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Button(
            onClick = onRetry,
            modifier = Modifier.weight(1f),
        ) { Text(t("common.try_again")) }
        OutlinedButton(
            onClick = onDone,
            modifier = Modifier.weight(1f),
        ) { Text(t("common.done")) }
    }
}

@Composable
private fun FailedPanel(
    onRetry: () -> Unit,
    onDone: () -> Unit,
) {
    Text(
        text = t("invite.contact.accept_failed"),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.error,
    )
    Spacer(Modifier.height(24.dp))
    Row(
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Button(
            onClick = onRetry,
            modifier = Modifier.weight(1f),
        ) { Text(t("common.retry")) }
        OutlinedButton(
            onClick = onDone,
            modifier = Modifier.weight(1f),
        ) { Text(t("common.done")) }
    }
}

