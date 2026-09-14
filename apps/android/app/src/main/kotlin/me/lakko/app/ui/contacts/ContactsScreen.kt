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
import androidx.compose.material3.AlertDialog
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
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.GeneratedInvite
import me.lakko.app.contacts.IncomingContactRequest
import me.lakko.app.contacts.SentContactRequest
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.profile.ResolvedName
import me.lakko.app.profile.requestCardTitle

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
    /** "Start call" on a call link this device created: hands the link's `/c/`
     *  slug to the JoinCall landing, where this account is the owner and hosts
     *  the call. `null` hides the button (tests). */
    onStartCallLink: ((slug: String) -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val callLinks by viewModel.callLinks.collectAsStateWithLifecycle()
    // ADR-0048 — every name on this screen comes from the one resolver; the
    // version flow recomposes a row in place when a profile arrives or a
    // username verifies.
    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()

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

    if (callLinks.formOpen) {
        CreateCallLinkDialog(
            state = callLinks,
            onMediaChange = viewModel::onCallLinkMediaChange,
            onExpiryChange = viewModel::onCallLinkExpiryChange,
            onMaxUsesChange = viewModel::onCallLinkMaxUsesChange,
            onLabelChange = viewModel::onCallLinkLabelChange,
            onCreate = viewModel::onCreateCallLink,
            onDismiss = viewModel::onDismissCallLinkForm,
        )
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

        val context = LocalContext.current
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
        ) {
            InviteSection(
                outstandingInvites = state.outstandingInvites,
                generating = state.generatingInvite,
                error = state.inviteError,
                onGenerateClicked = viewModel::onGenerateInviteClicked,
                onCopyClicked = { invite -> copyToClipboard(context, invite.url) },
                onShareClicked = { invite -> launchShareIntent(context, invite.url) },
                onRevokeClicked = { invite ->
                    viewModel.onRevokeInviteClicked(invite.blobId)
                },
            )

            Spacer(Modifier.height(24.dp))
            CallLinkSection(
                state = callLinks,
                onCreateClicked = viewModel::onOpenCallLinkForm,
                onCopyClicked = { link -> copyToClipboard(context, link.url) },
                onShareClicked = { link -> launchShareIntent(context, link.url) },
                onStartCallClicked = onStartCallLink?.let { start -> { link -> start(link.slug) } },
            )

            Spacer(Modifier.height(24.dp))
            ActiveInvitesSection(
                loaded = state.activeInvitesLoaded,
                invites = state.activeInvites,
                loading = state.loadingActiveInvites,
                error = state.activeInvitesError,
                revokingSlug = state.revokingActiveInviteSlug,
                onRefreshClicked = viewModel::onRefreshActiveInvitesClicked,
                onRevokeClicked = { slug ->
                    viewModel.onRevokeActiveInviteClicked(slug)
                },
            )

            Spacer(Modifier.height(24.dp))
            SendSection(
                usernameInput = state.usernameInput,
                usernameError = state.usernameError,
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
                nameOf = { key -> nameVersion.let { directory.resolve(key) } },
                onAccept = viewModel::onAcceptClicked,
                onDecline = viewModel::onDeclineClicked,
            )

            Spacer(Modifier.height(24.dp))
            AcceptedSection(
                accepted = state.accepted,
                nameOf = { key -> nameVersion.let { directory.resolve(key) } },
                onOpenDetail = { key -> directory.ensureVerified(key) },
                onOpenChat = onOpenChat,
                onRename = viewModel::onRenameContactClicked,
            )

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
// Call links (ADR-0032 `/c/<slug>`)
// =============================================================================

@Composable
private fun CallLinkSection(
    state: CallLinkUiState,
    onCreateClicked: () -> Unit,
    onCopyClicked: (CreatedCallLink) -> Unit,
    onShareClicked: (CreatedCallLink) -> Unit,
    onStartCallClicked: ((CreatedCallLink) -> Unit)?,
) {
    SectionHeader(title = "Call links", count = state.created.size)
    Spacer(Modifier.height(4.dp))
    Text(
        text = "Create a link anyone can open to join a call you host. The call " +
            "is not tied to a chat.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(8.dp))
    OutlinedButton(
        onClick = onCreateClicked,
        modifier = Modifier.fillMaxWidth(),
    ) { Text("Create call link") }

    if (state.created.isNotEmpty()) {
        Spacer(Modifier.height(12.dp))
        for (link in state.created) {
            CallLinkRow(
                link = link,
                onCopyClicked = { onCopyClicked(link) },
                onShareClicked = { onShareClicked(link) },
                onStartCallClicked = onStartCallClicked?.let { start -> { start(link) } },
            )
            Spacer(Modifier.height(8.dp))
        }
    }
}

@Composable
private fun CallLinkRow(
    link: CreatedCallLink,
    onCopyClicked: () -> Unit,
    onShareClicked: () -> Unit,
    onStartCallClicked: (() -> Unit)?,
) {
    Card(
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant,
        ),
        shape = RoundedCornerShape(8.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            val kind = if (link.video) "Video call" else "Audio call"
            Text(
                text = if (link.label.isEmpty()) kind else "${link.label} · $kind",
                style = MaterialTheme.typography.titleSmall,
            )
            Spacer(Modifier.height(4.dp))
            Text(
                text = link.url,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                maxLines = 2,
            )
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    onClick = onCopyClicked,
                    modifier = Modifier.weight(1f),
                ) { Text("Copy") }
                OutlinedButton(
                    onClick = onShareClicked,
                    modifier = Modifier.weight(1f),
                ) { Text("Share") }
                if (onStartCallClicked != null) {
                    Button(
                        onClick = onStartCallClicked,
                        modifier = Modifier.weight(1f),
                    ) { Text("Start call") }
                }
            }
        }
    }
}

@Composable
private fun CreateCallLinkDialog(
    state: CallLinkUiState,
    onMediaChange: (me.lakko.app.calls.CallMediaPreset) -> Unit,
    onExpiryChange: (me.lakko.app.calls.CallInviteExpiryPreset) -> Unit,
    onMaxUsesChange: (me.lakko.app.calls.CallInviteMaxUsesPreset) -> Unit,
    onLabelChange: (String) -> Unit,
    onCreate: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Create call link") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Call", style = MaterialTheme.typography.labelLarge)
                ChoiceRow(
                    options = listOf(
                        me.lakko.app.calls.CallMediaPreset.Audio to "Audio",
                        me.lakko.app.calls.CallMediaPreset.AudioVideo to "Video",
                    ),
                    selected = state.media,
                    enabled = !state.creating,
                    onSelect = onMediaChange,
                )
                Text("Expires after", style = MaterialTheme.typography.labelLarge)
                ChoiceRow(
                    options = listOf(
                        me.lakko.app.calls.CallInviteExpiryPreset.OneHour to "1 hour",
                        me.lakko.app.calls.CallInviteExpiryPreset.OneDay to "24 hours",
                        me.lakko.app.calls.CallInviteExpiryPreset.SevenDays to "7 days",
                    ),
                    selected = state.expiry,
                    enabled = !state.creating,
                    onSelect = onExpiryChange,
                )
                Text("Can be used", style = MaterialTheme.typography.labelLarge)
                ChoiceRow(
                    options = listOf(
                        me.lakko.app.calls.CallInviteMaxUsesPreset.Single to "Once",
                        me.lakko.app.calls.CallInviteMaxUsesPreset.Ten to "10 times",
                        me.lakko.app.calls.CallInviteMaxUsesPreset.Unlimited to "Unlimited",
                    ),
                    selected = state.maxUses,
                    enabled = !state.creating,
                    onSelect = onMaxUsesChange,
                )
                OutlinedTextField(
                    value = state.label,
                    onValueChange = onLabelChange,
                    label = { Text("Label (optional)") },
                    singleLine = true,
                    enabled = !state.creating,
                    keyboardOptions = KeyboardOptions(
                        capitalization = KeyboardCapitalization.Sentences,
                    ),
                    modifier = Modifier.fillMaxWidth(),
                )
                if (state.creating) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(
                            modifier = Modifier
                                .height(18.dp)
                                .width(18.dp),
                            strokeWidth = 2.dp,
                        )
                        Spacer(Modifier.width(8.dp))
                        Text(
                            "Creating the link. This can take a few seconds.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
                state.error?.let { error ->
                    Text(
                        text = error,
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
        },
        confirmButton = {
            Button(onClick = onCreate, enabled = !state.creating) {
                Text(if (state.creating) "Creating…" else "Create")
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !state.creating) { Text("Cancel") }
        },
    )
}

@Composable
private fun <T> ChoiceRow(
    options: List<Pair<T, String>>,
    selected: T,
    enabled: Boolean,
    onSelect: (T) -> Unit,
) {
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        for ((value, text) in options) {
            androidx.compose.material3.FilterChip(
                selected = value == selected,
                onClick = { onSelect(value) },
                label = { Text(text) },
                enabled = enabled,
            )
        }
    }
}

// =============================================================================
// Invite section — owner side (1.15e10)
// =============================================================================

@Composable
private fun InviteSection(
    outstandingInvites: List<GeneratedInvite>,
    generating: Boolean,
    error: String?,
    onGenerateClicked: () -> Unit,
    onCopyClicked: (GeneratedInvite) -> Unit,
    onShareClicked: (GeneratedInvite) -> Unit,
    onRevokeClicked: (GeneratedInvite) -> Unit,
) {
    SectionHeader(title = "Invite links", count = outstandingInvites.size)
    Spacer(Modifier.height(4.dp))
    Text(
        text = "Generate a single-use 7-day link your friend can open to add " +
            "you without typing your username.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(8.dp))

    Button(
        onClick = onGenerateClicked,
        enabled = !generating,
        modifier = Modifier.fillMaxWidth(),
    ) {
        if (generating) {
            CircularProgressIndicator(
                modifier = Modifier
                    .height(20.dp)
                    .width(20.dp),
                strokeWidth = 2.dp,
                color = MaterialTheme.colorScheme.onPrimary,
            )
            Spacer(Modifier.width(8.dp))
            Text("Generating…")
        } else {
            Text("Generate invite link")
        }
    }

    if (error != null) {
        Spacer(Modifier.height(8.dp))
        Text(
            text = error,
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodyMedium,
        )
    }

    if (outstandingInvites.isNotEmpty()) {
        Spacer(Modifier.height(12.dp))
        for (invite in outstandingInvites) {
            InviteRow(
                invite = invite,
                onCopyClicked = { onCopyClicked(invite) },
                onShareClicked = { onShareClicked(invite) },
                onRevokeClicked = { onRevokeClicked(invite) },
            )
            Spacer(Modifier.height(8.dp))
        }
    }
}

@Composable
private fun InviteRow(
    invite: GeneratedInvite,
    onCopyClicked: () -> Unit,
    onShareClicked: () -> Unit,
    onRevokeClicked: () -> Unit,
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
                text = invite.url,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                maxLines = 2,
            )
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = onCopyClicked,
                    modifier = Modifier.weight(1f),
                ) { Text("Copy") }
                Button(
                    onClick = onShareClicked,
                    modifier = Modifier.weight(1f),
                ) { Text("Share") }
                OutlinedButton(
                    onClick = onRevokeClicked,
                    modifier = Modifier.weight(1f),
                ) { Text("Revoke") }
            }
        }
    }
}

/** Phase 1.18d2b-list — server-authoritative "Active invites"
 *  section. Refreshed on explicit user gesture; the
 *  [InviteSection] above only tracks links generated in the
 *  current session. */
@Composable
private fun ActiveInvitesSection(
    loaded: Boolean,
    invites: List<me.lakko.app.discovery.ActiveInvite>,
    loading: Boolean,
    error: String?,
    revokingSlug: String?,
    onRefreshClicked: () -> Unit,
    onRevokeClicked: (String) -> Unit,
) {
    SectionHeader(
        title = "Active invites",
        count = if (loaded) invites.size else 0,
    )
    Spacer(Modifier.height(8.dp))
    Row(verticalAlignment = Alignment.CenterVertically) {
        Button(
            onClick = onRefreshClicked,
            enabled = !loading,
        ) {
            if (loading) {
                CircularProgressIndicator(
                    modifier = Modifier
                        .height(18.dp)
                        .width(18.dp),
                    strokeWidth = 2.dp,
                )
                Spacer(Modifier.width(8.dp))
            }
            Text(if (loading) "Loading…" else "Refresh")
        }
    }
    if (error != null) {
        Spacer(Modifier.height(8.dp))
        Text(
            text = error,
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodySmall,
        )
    }
    Spacer(Modifier.height(8.dp))
    if (!loaded) {
        EmptyHint("Tap Refresh to load your live invites.")
    } else if (invites.isEmpty()) {
        EmptyHint("No live invites — every link has been revoked or expired.")
    } else {
        for (row in invites) {
            ActiveInviteRow(
                row = row,
                revoking = revokingSlug == row.blobIdSlug,
                disabled = revokingSlug != null && revokingSlug != row.blobIdSlug,
                onRevoke = { onRevokeClicked(row.blobIdSlug) },
            )
            Spacer(Modifier.height(8.dp))
        }
    }
}

@Composable
private fun ActiveInviteRow(
    row: me.lakko.app.discovery.ActiveInvite,
    revoking: Boolean,
    disabled: Boolean,
    onRevoke: () -> Unit,
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
                text = row.label.ifEmpty { "(unlabelled)" },
                style = MaterialTheme.typography.bodyMedium,
            )
            Spacer(Modifier.height(4.dp))
            Text(
                text = row.blobIdSlug.take(16) + "… · " +
                    describeExpiry(row.expiryMs) + " · " +
                    describeUsage(row.usedCount, row.maxUses) +
                    if (row.bootstrapReferral) " · referral" else "",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontFamily = FontFamily.Monospace,
            )
            Spacer(Modifier.height(8.dp))
            OutlinedButton(
                onClick = onRevoke,
                enabled = !disabled && !revoking,
                modifier = Modifier.fillMaxWidth(),
            ) {
                if (revoking) {
                    CircularProgressIndicator(
                        modifier = Modifier
                            .height(16.dp)
                            .width(16.dp),
                        strokeWidth = 2.dp,
                    )
                    Spacer(Modifier.width(8.dp))
                    Text("Revoking…")
                } else {
                    Text("Revoke")
                }
            }
        }
    }
}

/** Human-readable rendering for `expiryMs`. `0` is the
 *  NEVER_EXPIRES sentinel; anything in the past renders as
 *  "expired" defensively. */
private fun describeExpiry(expiryMs: Long): String {
    if (expiryMs == 0L) return "never expires"
    val nowMs = System.currentTimeMillis()
    if (expiryMs <= nowMs) return "expired"
    val deltaMs = expiryMs - nowMs
    val days = deltaMs / (24L * 60 * 60 * 1000)
    if (days >= 1) return "expires in ${days}d"
    val hours = deltaMs / (60L * 60 * 1000)
    if (hours >= 1) return "expires in ${hours}h"
    val minutes = (deltaMs / (60L * 1000)).coerceAtLeast(1L)
    return "expires in ${minutes}m"
}

/** Sentinel mapping mirrors lakko-contacts: 0 = SINGLE_USE,
 *  0xFFFFFFFF = unlimited, any other N = N consumes allowed. */
private fun describeUsage(used: Long, max: Long): String {
    if (max == 0L) return if (used >= 1) "used 1/1" else "0/1 used"
    if (max == 0xFFFF_FFFFL) {
        return if (used == 1L) "1 use" else "$used uses"
    }
    return "$used/$max used"
}

/** Copy [text] to the system clipboard. Uses
 *  `ClipboardManager.setPrimaryClip` — Android 13+ surfaces the
 *  "copied to clipboard" snackbar automatically, so this
 *  helper doesn't need a Toast fallback. */
private fun copyToClipboard(context: Context, text: String) {
    val clipboard =
        context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
            ?: return
    val clip = ClipData.newPlainText("Lakko invite link", text)
    clipboard.setPrimaryClip(clip)
}

/** Launch the Android share sheet with [url] as the body.
 *  The user picks the destination app (messenger, email,
 *  SMS); we don't constrain it. */
private fun launchShareIntent(context: Context, url: String) {
    val send = Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_TEXT, url)
    }
    val chooser = Intent.createChooser(send, /* title = */ null)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    context.startActivity(chooser)
}

// =============================================================================
// Send section
// =============================================================================

@Composable
private fun SendSection(
    usernameInput: String,
    usernameError: String?,
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
        // ADR-0048 §4 — the rule's reason as the user types.
        isError = usernameError != null,
        supportingText = usernameError?.let { reason -> { Text(reason) } },
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
    nameOf: (ByteArray) -> ResolvedName,
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
        PendingRow(req = req, name = nameOf(req.fromPubkey), onAccept = onAccept, onDecline = onDecline)
        Spacer(Modifier.height(8.dp))
    }
}

@Composable
private fun PendingRow(
    req: IncomingContactRequest,
    name: ResolvedName,
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
            // ADR-0048 UX §5 — `Alice Petrova · @alice wants to connect`, or
            // `@alice` / the fingerprint when that is all there is.
            Text(
                text = requestCardTitle(name),
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = if (name.isFingerprint) FontFamily.Monospace else FontFamily.Default,
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
    nameOf: (ByteArray) -> ResolvedName,
    /** A contact detail opened: retry verifying its username if unresolved. */
    onOpenDetail: (signatureKeyB64: String) -> Unit,
    onOpenChat: ((groupIdHex: String) -> Unit)?,
    /** (messageId, newName) — a blank name clears the override
     *  (ADR-0021 follow-up: editable per-contact display name). */
    onRename: (messageId: Long, newName: String) -> Unit,
) {
    SectionHeader(title = "Accepted", count = accepted.size)
    if (accepted.isEmpty()) {
        EmptyHint(text = "No accepted contacts.")
        return
    }

    // A single rename dialog, driven by the contact currently being
    // edited (`null` = closed) + the draft name field.
    var editing by remember { mutableStateOf<AcceptedContact?>(null) }
    var draftName by remember { mutableStateOf("") }

    Spacer(Modifier.height(8.dp))
    for (c in accepted) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 4.dp),
        ) {
            val name = nameOf(c.request.fromPubkey)
            Column(modifier = Modifier.weight(1f)) {
                // ADR-0048 UX §3 — the resolved name (rename, profile name,
                // verified @username, fingerprint); monospaced when it IS the
                // fingerprint, so it reads as a key rather than a name.
                Text(
                    text = name.primary,
                    style = MaterialTheme.typography.bodyMedium,
                    fontFamily = if (name.isFingerprint) FontFamily.Monospace else FontFamily.Default,
                )
                // The verified username (or the fingerprint) under the name
                // disambiguates two contacts with the same name.
                if (!name.isFingerprint) {
                    Text(
                        text = name.verifiedUsername?.let { "@$it" } ?: name.fingerprint,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = if (name.verifiedUsername != null) {
                            FontFamily.Default
                        } else {
                            FontFamily.Monospace
                        },
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            TextButton(
                onClick = {
                    draftName = c.displayName ?: ""
                    onOpenDetail(java.util.Base64.getEncoder().encodeToString(c.request.fromPubkey))
                    editing = c
                },
            ) {
                Text("Details")
            }
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

    val target = editing
    if (target != null) {
        val detail = nameOf(target.request.fromPubkey)
        AlertDialog(
            onDismissRequest = { editing = null },
            title = { Text(detail.primary) },
            text = {
                Column {
                    // ADR-0048 UX §6 — the contact detail: username (only when
                    // verified), the person's own name when a rename overrides
                    // it, a quiet line when a username claim failed, and the
                    // fingerprint labelled as such.
                    detail.verifiedUsername?.let { user ->
                        Text(text = "@$user", style = MaterialTheme.typography.bodyMedium)
                    }
                    detail.theirName?.let { theirs ->
                        Text(text = "Their name: $theirs", style = MaterialTheme.typography.bodySmall)
                    }
                    if (detail.usernameNotVerified) {
                        Text(
                            text = "Username not verified",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Text(
                        text = "Fingerprint ${detail.fingerprint}",
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.height(12.dp))
                    Text(
                        text = "Your name for this contact. Only you see it; " +
                            "leave blank to use their own name.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    Spacer(Modifier.height(8.dp))
                    OutlinedTextField(
                        value = draftName,
                        onValueChange = { draftName = it },
                        singleLine = true,
                        label = { Text("Rename") },
                    )
                }
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        onRename(target.request.messageId, draftName)
                        editing = null
                    },
                ) {
                    Text("Save")
                }
            },
            dismissButton = {
                TextButton(onClick = { editing = null }) {
                    Text("Close")
                }
            },
        )
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
