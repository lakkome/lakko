// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import androidx.compose.foundation.ExperimentalFoundationApi
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
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.PersonAdd
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.contacts.GeneratedInvite
import me.lakko.app.contacts.IncomingContactRequest
import me.lakko.app.contacts.SentContactRequest
import me.lakko.app.discovery.ActiveInvite
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.profile.ResolvedName
import me.lakko.app.profile.requestSenderLabel
import me.lakko.app.ui.components.LakkoAvatar
import java.text.DateFormat
import java.util.Date

/**
 * The Contacts tab — docs/ux/client-redesign.md section 5.3.
 *
 * Top bar with the title, a search field and one add action whose menu holds
 * Add by username and Share invite link (the QR entries of the spec are
 * omitted: this client has no contact-QR payload, only the device-pairing one).
 * Below: a Requests section (incoming rows with Accept and Decline; sent
 * requests collapsed under "Sent"), then the alphabetical contact list with
 * sticky letter headers. Pull-to-refresh runs the same inbox poll the screen
 * already runs continuously.
 *
 * Add by username lives in a bottom sheet; invite links are managed in an
 * "Invite links" sheet (copy, share, revoke, create).
 *
 * Call links are not here at all. They were, briefly, and the create-a-link
 * form that held them is gone from every screen: a link now belongs to a call
 * that is already happening and is offered from inside it
 * (`docs/ux/call-links.md` sections 1 and 6).
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
public fun ContactsScreen(
    viewModel: ContactsViewModel,
    /** Open a contact's detail by its route key (base64url signature key).
     *  `null` makes a row open the chat directly when it has one (tests). */
    onOpenContact: ((key: String) -> Unit)? = null,
    /** Open the 1x1 chat for an accepted contact (fallback when
     *  [onOpenContact] is not wired). */
    onOpenChat: ((groupIdHex: String) -> Unit)? = null,
    /** docs/ux/navigation.md section 4.3 — a username the conversation list's
     *  one lookup row resolved. The add sheet opens with it already filled in,
     *  because the person has typed it once and the request flow is the only
     *  thing here they have not done yet. */
    prefillUsername: String? = null,
    /** Called once [prefillUsername] has been taken, so the slot can be
     *  emptied and a later visit does not re-open the sheet. */
    onPrefillConsumed: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    // ADR-0048 — every name on this screen comes from the one resolver; the
    // version flow recomposes a row in place when a profile arrives or a
    // username verifies.
    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val context = LocalContext.current

    // Refresh on arrival only. The continuous long poll moved to the session
    // root (`SessionRoot` in `MainActivity`), because the account mailbox
    // carries contact requests, acknowledgements and the Welcome that makes a
    // peer's 1x1 chat appear — none of which may wait for this screen to be
    // open. Two continuous readers would race the repository's single cursor,
    // so this one is a one-shot.
    LaunchedEffect(Unit) {
        viewModel.onRefreshClicked()
    }

    var query by remember { mutableStateOf("") }
    var menuOpen by remember { mutableStateOf(false) }
    var addSheetOpen by remember { mutableStateOf(false) }
    var inviteSheetOpen by remember { mutableStateOf(false) }
    var sentExpanded by remember { mutableStateOf(false) }

    // docs/ux/navigation.md section 4.3 — a name found by the conversation
    // list's lookup row opens the add sheet already filled in. Consumed as soon
    // as it is taken, so returning to this tab later does not re-open it.
    LaunchedEffect(prefillUsername) {
        val username = prefillUsername ?: return@LaunchedEffect
        viewModel.onUsernameChange(username)
        addSheetOpen = true
        onPrefillConsumed?.invoke()
    }

    if (addSheetOpen) {
        AddByUsernameSheet(
            state = state,
            onUsernameChange = viewModel::onUsernameChange,
            onIntroChange = viewModel::onIntroChange,
            onSend = viewModel::onSendClicked,
            onSent = {
                viewModel.consumeSendSuccess()
                addSheetOpen = false
                sentExpanded = true
            },
            onDismiss = {
                viewModel.onDismissAddSheet()
                addSheetOpen = false
            },
        )
    }

    if (inviteSheetOpen) {
        InviteLinksSheet(
            state = state,
            onLoad = viewModel::onRefreshActiveInvitesClicked,
            onCreate = viewModel::onGenerateInviteClicked,
            onCopy = { url -> copyToClipboard(context, url) },
            onShare = { url -> launchShareIntent(context, url) },
            onRevoke = { row ->
                val session = state.outstandingInvites.firstOrNull { it.blobIdSlug == row.slug }
                val sessionBlobId = session?.blobId
                if (session != null && sessionBlobId != null) {
                    viewModel.onRevokeInviteClicked(sessionBlobId)
                } else if (session != null) {
                    // An inline link: no ledger row to revoke.
                    viewModel.onForgetInlineInviteClicked(session.blobIdSlug)
                } else {
                    viewModel.onRevokeActiveInviteClicked(row.slug)
                }
            },
            onDismiss = { inviteSheetOpen = false },
        )
    }

    // The rows of the alphabetical list: resolved, filtered, sectioned.
    // Defensive: rows persisted before the accept path deduplicated by key
    // may still hold one person twice, and LazyColumn keys must be unique.
    val accepted = state.accepted.distinctBy { contactRouteKey(it.request.fromPubkey) }
    val sections = remember(accepted, nameVersion, query) {
        val rows = accepted.map { c ->
            val resolved = directory.resolve(c.request.fromPubkey)
            ContactRowModel(
                key = contactRouteKey(c.request.fromPubkey),
                name = resolved.primary,
                username = resolved.verifiedUsername,
                placeholder = resolved.isPlaceholder,
            )
        }
        contactSections(filterContacts(rows, query))
    }
    val byKey = remember(accepted) { accepted.associateBy { contactRouteKey(it.request.fromPubkey) } }
    val pending = remember(state.pending) { sortRequestsNewestFirst(state.pending) { it.messageId } }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text("Contacts") },
                actions = {
                    Box {
                        IconButton(onClick = { menuOpen = true }) {
                            Icon(Icons.Filled.PersonAdd, contentDescription = "Add contact")
                        }
                        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                            DropdownMenuItem(
                                text = { Text("Add by username") },
                                leadingIcon = { Icon(Icons.Filled.PersonAdd, contentDescription = null) },
                                onClick = { menuOpen = false; addSheetOpen = true },
                            )
                            DropdownMenuItem(
                                text = { Text("Share invite link") },
                                leadingIcon = { Icon(Icons.Filled.Link, contentDescription = null) },
                                onClick = { menuOpen = false; inviteSheetOpen = true },
                            )
                        }
                    }
                },
            )
        },
    ) { padding ->
        PullToRefreshBox(
            isRefreshing = state.refreshing,
            onRefresh = viewModel::onRefreshClicked,
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            if (state.requiresReauth) {
                ReauthPanel()
                return@PullToRefreshBox
            }
            val nothingAtAll = accepted.isEmpty() && pending.isEmpty() && state.sent.isEmpty()
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                item(key = "search") {
                    SearchField(
                        query = query,
                        onQueryChange = { query = it },
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    )
                }
                if (state.refreshError != null) {
                    // The transport detail stays out of the copy (UX section 6).
                    item(key = "refresh-error") {
                        Text(
                            text = "Could not check for new requests. Pull down to retry.",
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                        )
                    }
                }

                if (pending.isNotEmpty()) {
                    item(key = "requests-header") { SectionTitle("Requests") }
                    items(pending.distinctBy { it.messageId }, key = { "req-${it.messageId}" }) { req ->
                        RequestRow(
                            req = req,
                            name = nameVersion.let { directory.resolve(req.fromPubkey) },
                            onAccept = { viewModel.onAcceptClicked(req.messageId) },
                            onDecline = { viewModel.onDeclineClicked(req.messageId) },
                        )
                    }
                }

                if (state.sent.isNotEmpty()) {
                    item(key = "sent-header") {
                        SentHeader(
                            count = state.sent.size,
                            expanded = sentExpanded,
                            onToggle = { sentExpanded = !sentExpanded },
                        )
                    }
                    if (sentExpanded) {
                        items(state.sent.distinctBy { it.messageId }, key = { "sent-${it.messageId}" }) { s -> SentRow(s) }
                    }
                }

                if (nothingAtAll && query.isBlank()) {
                    item(key = "empty") {
                        EmptyState(onAdd = { addSheetOpen = true })
                    }
                } else if (sections.isEmpty() && query.isNotBlank()) {
                    item(key = "no-match") {
                        Text(
                            "No contacts match.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(16.dp),
                        )
                    }
                } else {
                    if (pending.isNotEmpty() || state.sent.isNotEmpty()) {
                        item(key = "contacts-header") { SectionTitle("Contacts") }
                    }
                    for (section in sections) {
                        stickyHeader(key = "letter-${section.letter}") { LetterHeader(section.letter) }
                        items(section.rows, key = { "c-${it.key}" }) { row ->
                            val contact = byKey[row.key]
                            ContactRow(
                                row = row,
                                onClick = {
                                    when {
                                        onOpenContact != null -> onOpenContact(row.key)
                                        contact?.groupIdHex != null && onOpenChat != null ->
                                            onOpenChat(contact.groupIdHex)
                                    }
                                },
                            )
                        }
                    }
                    item(key = "tail") { Spacer(Modifier.height(24.dp)) }
                }
            }
        }
    }
}

// =============================================================================
// List pieces
// =============================================================================

@Composable
private fun SearchField(query: String, onQueryChange: (String) -> Unit, modifier: Modifier = Modifier) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier
            .fillMaxWidth()
            .clip(CircleShape)
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .padding(horizontal = 12.dp, vertical = 4.dp),
    ) {
        Icon(
            Icons.Filled.Search,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.width(8.dp))
        androidx.compose.foundation.text.BasicTextField(
            value = query,
            onValueChange = onQueryChange,
            singleLine = true,
            textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
            cursorBrush = androidx.compose.ui.graphics.SolidColor(MaterialTheme.colorScheme.primary),
            decorationBox = { inner ->
                Box(modifier = Modifier.padding(vertical = 8.dp)) {
                    if (query.isEmpty()) {
                        Text(
                            "Search contacts",
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    inner()
                }
            },
            modifier = Modifier.weight(1f),
        )
        if (query.isNotEmpty()) {
            IconButton(onClick = { onQueryChange("") }, modifier = Modifier.size(32.dp)) {
                Icon(
                    Icons.Filled.Close,
                    contentDescription = "Clear search",
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun SectionTitle(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun LetterHeader(letter: String) {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.background)
            .padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Text(
            text = letter,
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ContactRow(row: ContactRowModel, onClick: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        LakkoAvatar(name = row.name, seed = row.key, size = 40.dp)
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(row.name, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
            row.username?.takeIf { row.name != "@$it" }?.let { user ->
                Text(
                    "@$user",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun RequestRow(
    req: IncomingContactRequest,
    name: ResolvedName,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
) {
    val title = requestSenderLabel(name)
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            LakkoAvatar(
                name = if (name.isPlaceholder) "?" else name.primary,
                seed = contactRouteKey(req.fromPubkey),
                size = 40.dp,
            )
            Spacer(Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
                Text(
                    "Wants to connect",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (!req.intro.isNullOrBlank()) {
            Spacer(Modifier.height(6.dp))
            Text(
                text = req.intro,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(start = 52.dp),
            )
        }
        Spacer(Modifier.height(8.dp))
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.padding(start = 52.dp),
        ) {
            Button(onClick = onAccept) { Text("Accept") }
            OutlinedButton(onClick = onDecline) { Text("Decline") }
        }
    }
}

@Composable
private fun SentHeader(count: Int, expanded: Boolean, onToggle: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onToggle)
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Text(
            text = "Sent ($count)",
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.weight(1f),
        )
        Icon(
            imageVector = if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
            contentDescription = if (expanded) "Collapse" else "Expand",
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun SentRow(s: SentContactRequest) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 6.dp),
    ) {
        LakkoAvatar(name = s.recipientUsername, seed = s.recipientAccountIdB64, size = 40.dp)
        Spacer(Modifier.width(12.dp))
        Text(
            "@${s.recipientUsername}",
            style = MaterialTheme.typography.bodyLarge,
            modifier = Modifier.weight(1f),
        )
        Text(
            "Waiting",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun EmptyState(onAdd: () -> Unit) {
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 24.dp, vertical = 48.dp),
    ) {
        Text("No contacts yet", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(12.dp))
        Button(onClick = onAdd) { Text("Add contact") }
    }
}

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
            text = "Log out and log back in to load your contacts.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

// =============================================================================
// Add by username — bottom sheet
// =============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AddByUsernameSheet(
    state: ContactsUiState,
    onUsernameChange: (String) -> Unit,
    onIntroChange: (String) -> Unit,
    onSend: () -> Unit,
    onSent: () -> Unit,
    onDismiss: () -> Unit,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    LaunchedEffect(state.sendSucceeded) {
        if (state.sendSucceeded) onSent()
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .imePadding()
                .navigationBarsPadding()
                .padding(horizontal = 24.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("Add by username", style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(
                value = state.usernameInput,
                onValueChange = onUsernameChange,
                label = { Text("Username") },
                prefix = { Text("@") },
                singleLine = true,
                enabled = !state.sending,
                isError = state.usernameError != null,
                supportingText = state.usernameError?.let { reason -> { Text(reason) } },
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Text,
                ),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = state.introInput,
                onValueChange = onIntroChange,
                label = { Text("Say hi (optional)") },
                enabled = !state.sending,
                minLines = 2,
                modifier = Modifier.fillMaxWidth(),
            )
            state.sendError?.let { error ->
                Text(
                    text = error,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Button(
                onClick = onSend,
                enabled = !state.sending && state.usernameInput.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) {
                if (state.sending) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(20.dp),
                        strokeWidth = 2.dp,
                        color = MaterialTheme.colorScheme.onPrimary,
                    )
                    Spacer(Modifier.width(8.dp))
                    Text("Sending...")
                } else {
                    Text("Send request")
                }
            }
        }
    }
}

// =============================================================================
// Invite links — bottom sheet
// =============================================================================

private fun GeneratedInvite.toRow(): InviteLinkRow = InviteLinkRow(
    slug = blobIdSlug,
    url = url,
    label = label,
    createdAtMs = createdAtMs,
    expiryMs = expiryMs,
    usesLeft = invitesUsesLeft(maxUses, 0L),
    fromSession = true,
)

private fun ActiveInvite.toRow(): InviteLinkRow = InviteLinkRow(
    slug = blobIdSlug,
    // The server lists the row but never held the link secret, so a link
    // made on another device or in an earlier session cannot be copied
    // again (sealed-link amendment of 2026-09-28); it can be revoked.
    url = null,
    label = label,
    createdAtMs = null,
    expiryMs = expiryMs,
    usesLeft = invitesUsesLeft(maxUses, usedCount),
    fromSession = false,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun InviteLinksSheet(
    state: ContactsUiState,
    onLoad: () -> Unit,
    onCreate: () -> Unit,
    onCopy: (String) -> Unit,
    onShare: (String) -> Unit,
    onRevoke: (InviteLinkRow) -> Unit,
    onDismiss: () -> Unit,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    // The server's list loads when the sheet opens; there is no Refresh button.
    LaunchedEffect(Unit) { onLoad() }
    val rows = remember(state.outstandingInvites, state.activeInvites) {
        mergeInviteLinkRows(
            session = state.outstandingInvites.map { it.toRow() },
            server = state.activeInvites.map { it.toRow() },
        )
    }
    val now = remember(rows) { System.currentTimeMillis() }
    val dateFormat = remember { DateFormat.getDateInstance(DateFormat.MEDIUM) }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .padding(horizontal = 24.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("Invite links", style = MaterialTheme.typography.titleLarge)
            Text(
                "Anyone who opens a link can add you without typing your username.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            (state.inviteError ?: state.activeInvitesError)?.let { error ->
                Text(error, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
            }
            if (state.loadingActiveInvites && rows.isEmpty()) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(8.dp))
                    Text("Loading...", style = MaterialTheme.typography.bodySmall)
                }
            } else if (rows.isEmpty() && state.activeInvitesLoaded) {
                Text(
                    "No active links.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            LazyColumn(
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f, fill = false),
            ) {
                items(rows, key = { it.slug }) { row ->
                    val meta = buildList {
                        row.createdAtMs?.let { add("Created ${dateFormat.format(Date(it))}") }
                        add(describeInviteExpiry(row.expiryMs, now))
                        describeUsesLeft(row.usesLeft)?.let { add(it) }
                    }.joinToString(" · ")
                    LinkRow(
                        title = row.label.ifBlank { "Invite link" },
                        meta = meta,
                        onCopy = row.url?.let { url -> { onCopy(url) } },
                        onShare = row.url?.let { url -> { onShare(url) } },
                        trailing = {
                            val revoking = state.revokingActiveInviteSlug == row.slug
                            TextButton(
                                onClick = { onRevoke(row) },
                                enabled = state.revokingActiveInviteSlug == null,
                            ) {
                                Text(
                                    if (revoking) "Revoking..." else "Revoke",
                                    color = MaterialTheme.colorScheme.error,
                                )
                            }
                        },
                    )
                    HorizontalDivider()
                }
            }
            Button(
                onClick = onCreate,
                enabled = !state.generatingInvite,
                modifier = Modifier.fillMaxWidth(),
            ) {
                if (state.generatingInvite) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(20.dp),
                        strokeWidth = 2.dp,
                        color = MaterialTheme.colorScheme.onPrimary,
                    )
                    Spacer(Modifier.width(8.dp))
                    Text("Creating...")
                } else {
                    Text("Create link")
                }
            }
        }
    }
}

/** A link row: title, meta line, copy and share icons, an optional trailing
 *  action. The URL itself is never shown as text (UX 5.3). */
@Composable
private fun LinkRow(
    title: String,
    meta: String,
    onCopy: (() -> Unit)?,
    onShare: (() -> Unit)?,
    trailing: @Composable () -> Unit = {},
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
            if (meta.isNotEmpty()) {
                Text(
                    meta,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (onCopy != null) {
            IconButton(onClick = onCopy) {
                Icon(Icons.Filled.ContentCopy, contentDescription = "Copy link")
            }
        }
        if (onShare != null) {
            IconButton(onClick = onShare) {
                Icon(Icons.Filled.Share, contentDescription = "Share link")
            }
        }
        trailing()
    }
}

// =============================================================================
// System helpers
// =============================================================================

/** Copy [text] to the system clipboard. Android 13+ surfaces its own
 *  "copied" confirmation, so no toast is needed. */
internal fun copyToClipboard(context: Context, text: String) {
    val clipboard =
        context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
            ?: return
    clipboard.setPrimaryClip(ClipData.newPlainText("Lakko link", text))
}

/** Launch the Android share sheet with [url] as the body. */
internal fun launchShareIntent(context: Context, url: String) {
    val send = Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_TEXT, url)
    }
    val chooser = Intent.createChooser(send, /* title = */ null)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    context.startActivity(chooser)
}

