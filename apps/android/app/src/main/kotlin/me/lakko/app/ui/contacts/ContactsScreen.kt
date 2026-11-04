// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.PersonAdd
import androidx.compose.material.icons.filled.QrCode
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material.icons.filled.Search
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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.contacts.GeneratedInvite
import me.lakko.app.l10n.joinFacts
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.contacts.IncomingContactRequest
import me.lakko.app.contacts.PendingContacts
import me.lakko.app.contacts.SentContactRequest
import me.lakko.app.auth.base64Encode
import me.lakko.app.discovery.ActiveInvite
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.profile.ResolvedName
import me.lakko.app.profile.requestSenderLabel
import me.lakko.app.ui.chat.PeopleSearchSection
import me.lakko.app.ui.components.LakkoAvatar

/**
 * The Contacts tab — docs/ux/client-redesign.md section 5.3.
 *
 * Top bar with the title, a search field and one add action whose menu holds
 * Share invite link and Scan QR code (a scanned `/i/`, `/g/` or `/c/` link
 * takes the route an opened app link takes; an invite link's own QR is shown
 * from its row in the Invite links sheet). The search field is
 * the one people search of `docs/ux/who-can-find-me.md` section 6.1: it
 * filters the contacts and, from 3 characters, asks the network for people
 * on its own, listed under **People** with **Add contact** on each row.
 * Below: a Requests section (incoming rows with Accept and Decline; sent
 * requests collapsed under "Sent"), then the alphabetical contact list with
 * sticky letter headers. Pull-to-refresh runs the same inbox poll the screen
 * already runs continuously.
 *
 * Invite links are managed in an "Invite links" sheet (create with the
 * defaults, copy, QR code, edit while unshared, delete).
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
    /** ADR-0079 — open the chat with a person added through their invite
     *  link whose app has not answered yet (by account id hex). */
    onOpenPendingChat: ((accountIdHex: String) -> Unit)? = null,
    /** `docs/ux/who-can-find-me.md` section 6.1 — people under the search
     *  field, local and network; `null` filters the contacts only (tests). */
    peopleSearch: me.lakko.app.search.PeopleSearchService? = null,
    /** A Lakko `/i/`, `/g/` or `/c/` link read off a scanned QR code, handed
     *  to the same intake an opened app link uses; `null` hides Scan QR code
     *  (tests). */
    onScannedLink: ((me.lakko.app.contacts.LakkoLink) -> Unit)? = null,
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
    var inviteSheetOpen by remember { mutableStateOf(false) }
    var scanSheetOpen by remember { mutableStateOf(false) }
    var sentExpanded by remember { mutableStateOf(false) }

    // docs/ux/who-can-find-me.md section 6.1 — the field asks the network on
    // its own (3 characters, 500 ms after the last keystroke).
    val scope = rememberCoroutineScope()
    val peopleSession = remember(peopleSearch, scope) { peopleSearch?.session(scope) }
    LaunchedEffect(query, peopleSession) { peopleSession?.onQuery(query) }

    if (inviteSheetOpen) {
        val linkRecords by remember(viewModel) { viewModel.inviteLinkRecords() }.collectAsStateWithLifecycle()
        InviteLinksSheet(
            state = state,
            linkSync = remember(linkRecords) { inviteLinkSyncBySlug(linkRecords) },
            onLoad = viewModel::onRefreshActiveInvitesClicked,
            onCreate = { viewModel.onGenerateInviteClicked() },
            onSaveEdit = viewModel::onEditInviteSaved,
            onCopy = { row, url -> viewModel.onCopyInviteClicked(row.slug, url) { copyToClipboard(context, it) } },
            onDelete = { row -> viewModel.onDeleteInviteClicked(row.slug) },
            onDismiss = { inviteSheetOpen = false },
        )
    }
    if (scanSheetOpen && onScannedLink != null) {
        ScanLinkSheet(
            onLink = { link ->
                scanSheetOpen = false
                onScannedLink(link)
            },
            onDismiss = { scanSheetOpen = false },
        )
    }

    // The rows of the alphabetical list: resolved, filtered, sectioned.
    // Defensive: rows persisted before the accept path deduplicated by key
    // may still hold one person twice, and LazyColumn keys must be unique.
    val accepted = state.accepted.distinctBy { contactRouteKey(it.request.fromPubkey) }
    // ADR-0079 section 4 — a person added through their invite link is listed
    // at once, under the name the link states, mixed in alphabetically.
    val invitedAll by viewModel.invited.collectAsStateWithLifecycle()
    val invited = remember(invitedAll, accepted) {
        PendingContacts.shown(invitedAll, accepted.mapTo(HashSet()) { base64Encode(it.request.fromPubkey) })
    }
    val unnamed = t("invite.contact.page.unnamed")
    val sections = remember(accepted, invited, nameVersion, query, unnamed) {
        val rows = accepted.map { c ->
            val resolved = directory.resolve(c.request.fromPubkey)
            ContactRowModel(
                key = contactRouteKey(c.request.fromPubkey),
                name = resolved.primary,
                username = resolved.verifiedUsername,
                placeholder = resolved.isPlaceholder,
            )
        } + invited.map { p ->
            val stated = PendingContacts.name(p)
            ContactRowModel(
                key = INVITED_ROW_PREFIX + p.accountIdHex,
                name = stated ?: unnamed,
                username = p.username.ifEmpty { null },
                placeholder = stated == null,
                personKey = p.signaturePubkeyB64,
            )
        }
        contactSections(filterContacts(rows, query))
    }
    val byKey = remember(accepted) { accepted.associateBy { contactRouteKey(it.request.fromPubkey) } }
    // UX who-can-find-me 6.1 — **Open** on a people row opens the conversation
    // (the contact's page when there is none yet).
    val openPerson: (String) -> Unit = { keyB64 ->
        val key = runCatching { java.util.Base64.getDecoder().decode(keyB64) }.getOrNull()
        val contact = key?.let { k -> accepted.firstOrNull { it.request.fromPubkey.contentEquals(k) } }
        when {
            contact?.groupIdHex != null && onOpenChat != null -> onOpenChat(contact.groupIdHex)
            key != null && onOpenContact != null -> onOpenContact(contactRouteKey(key))
        }
    }
    // The people the filter lists above the People part, contacts and pending
    // contacts alike; the People part leaves them out.
    val listedKeys = remember(sections, byKey) {
        listedPersonKeys(sections) { rowKey -> byKey[rowKey]?.let { base64Encode(it.request.fromPubkey) } }
    }
    val pending = remember(state.pending) { sortRequestsNewestFirst(state.pending) { it.messageId } }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("nav.contacts")) },
                actions = {
                    Box {
                        IconButton(onClick = { menuOpen = true }) {
                            Icon(Icons.Filled.PersonAdd, contentDescription = t("contacts.list.add"))
                        }
                        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                            DropdownMenuItem(
                                text = { Text(t("contacts.add.share_invite")) },
                                leadingIcon = { Icon(Icons.Filled.Link, contentDescription = null) },
                                onClick = { menuOpen = false; inviteSheetOpen = true },
                            )
                            if (onScannedLink != null) {
                                DropdownMenuItem(
                                    text = { Text(t("contacts.scan_qr")) },
                                    leadingIcon = { Icon(Icons.Filled.QrCodeScanner, contentDescription = null) },
                                    onClick = { menuOpen = false; scanSheetOpen = true },
                                )
                            }
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
            val nothingAtAll = accepted.isEmpty() && invited.isEmpty() && pending.isEmpty() && state.sent.isEmpty()
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                item(key = "search") {
                    SearchField(
                        query = query,
                        onQueryChange = { query = it },
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    )
                }
                state.refreshError?.let { inboxError ->
                    // The transport detail stays out of the copy (UX section 6);
                    // the sentence follows the error's kind (ADR-0061 section 6).
                    item(key = "refresh-error") {
                        Text(
                            text = t(inboxError.key),
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                        )
                    }
                }

                // who-can-find-me.md section 6.1, "One list": while a text is
                // typed the field shows one list with no headings, so the
                // request blocks and the letter headers step aside.
                val oneList = query.isNotBlank()
                if (pending.isNotEmpty() && !oneList) {
                    item(key = "requests-header") { SectionTitle(t("contacts.list.requests")) }
                    items(pending.distinctBy { it.messageId }, key = { "req-${it.messageId}" }) { req ->
                        RequestRow(
                            req = req,
                            name = nameVersion.let { directory.resolve(req.fromPubkey) },
                            accepting = req.messageId in state.acceptingIds,
                            acceptFailed = req.messageId in state.acceptFailedIds,
                            onAccept = { viewModel.onAcceptClicked(req.messageId) },
                            onDecline = { viewModel.onDeclineClicked(req.messageId) },
                        )
                    }
                }

                if (state.sent.isNotEmpty() && !oneList) {
                    item(key = "sent-header") {
                        SentHeader(
                            count = state.sent.size,
                            expanded = sentExpanded,
                            onToggle = { sentExpanded = !sentExpanded },
                        )
                    }
                    if (sentExpanded) {
                        items(state.sent.distinctBy { it.messageId }, key = { "sent-${it.messageId}" }) { s ->
                            SentRow(s, onDismiss = { viewModel.onDismissSentClicked(s) })
                        }
                    }
                }

                if (nothingAtAll && query.isBlank()) {
                    item(key = "empty") {
                        EmptyState(onAdd = { inviteSheetOpen = true })
                    }
                } else if (sections.isEmpty() && query.isNotBlank()) {
                    item(key = "no-match") {
                        if (peopleSearch != null && peopleSession != null) {
                            PeopleSearchSection(
                                query = query,
                                service = peopleSearch,
                                session = peopleSession,
                                conversationMatches = 0,
                                onOpenChannel = { _, _ -> },
                                onOpenContact = openPerson,
                                noLocalMatchKey = "search.nothing_found",
                                excludeKeysB64 = listedKeys,
                            )
                        } else {
                            Text(
                                t("contacts.list.no_match"),
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(16.dp),
                            )
                        }
                    }
                } else {
                    if ((pending.isNotEmpty() || state.sent.isNotEmpty()) && !oneList) {
                        item(key = "contacts-header") { SectionTitle(t("nav.contacts")) }
                    }
                    for (section in sections) {
                        if (!oneList) stickyHeader(key = "letter-${section.letter}") { LetterHeader(section.letter) }
                        items(section.rows, key = { "c-${it.key}" }) { row ->
                            val contact = byKey[row.key]
                            ContactRow(
                                row = row,
                                onClick = {
                                    when {
                                        row.key.startsWith(INVITED_ROW_PREFIX) ->
                                            onOpenPendingChat?.invoke(row.key.removePrefix(INVITED_ROW_PREFIX))
                                        onOpenContact != null -> onOpenContact(row.key)
                                        contact?.groupIdHex != null && onOpenChat != null ->
                                            onOpenChat(contact.groupIdHex)
                                    }
                                },
                            )
                        }
                    }
                    if (query.isNotBlank() && peopleSearch != null && peopleSession != null) {
                        item(key = "people") {
                            PeopleSearchSection(
                                query = query,
                                service = peopleSearch,
                                session = peopleSession,
                                conversationMatches = sections.sumOf { it.rows.size },
                                onOpenChannel = { _, _ -> },
                                onOpenContact = openPerson,
                                noLocalMatchKey = null,
                                excludeKeysB64 = listedKeys,
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

/** The row key prefix of a pending contact (ADR-0079); a contact's key is a
 *  base64url signature key, which never contains `:`. */
private const val INVITED_ROW_PREFIX = "invited:"

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
                            t("contacts.list.search"),
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
                    contentDescription = t("chats.search.clear"),
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
        LakkoAvatar(
            name = row.name,
            seed = row.key,
            size = 40.dp,
            personKey = me.lakko.app.ui.components.personKeyBytes(row.personKey ?: row.key),
        )
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
    accepting: Boolean,
    acceptFailed: Boolean,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
) {
    // The resolver's "Someone" placeholder is a data value; the screen says it in the interface language.
    val title = if (name.isPlaceholder) t("contacts.request.someone") else requestSenderLabel(name)
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
                personKey = req.fromPubkey,
            )
            Spacer(Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
                Text(
                    t("contacts.request.no_intro"),
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
            Button(onClick = onAccept, enabled = !accepting) { Text(t("contacts.request.accept")) }
            OutlinedButton(onClick = onDecline, enabled = !accepting) { Text(t("contacts.request.decline")) }
        }
        if (acceptFailed) {
            Spacer(Modifier.height(6.dp))
            Text(
                text = t("contacts.request.accept_failed"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(start = 52.dp),
            )
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
            text = t("contacts.list.sent", mapOf("count" to count.toString())),
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.weight(1f),
        )
        Icon(
            imageVector = if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
            contentDescription = if (expanded) t("common.collapse") else t("common.expand"),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun SentRow(s: SentContactRequest, onDismiss: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 6.dp),
    ) {
        val text = sentRowText(s.recipientName, s.recipientUsername)
        LakkoAvatar(
            name = text.title.trimStart('@'),
            seed = s.recipientAccountIdB64,
            size = 40.dp,
            personKey = me.lakko.app.ui.components.personKeyBytes(s.recipientKeyB64),
        )
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(text.title, style = MaterialTheme.typography.bodyLarge)
            text.username?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(
                t("contacts.list.sent_waiting"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        val dismissLabel = t("contacts.list.sent_dismiss")
        TextButton(
            onClick = onDismiss,
            modifier = Modifier.semantics { contentDescription = dismissLabel },
        ) { Text(t("common.dismiss")) }
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
        Text(t("contacts.list.empty.title"), style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(4.dp))
        Text(
            t("contacts.list.empty.body"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
        Spacer(Modifier.height(12.dp))
        Button(onClick = onAdd) { Text(t("contacts.add.share_invite")) }
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
            text = t("chat.reauth.title"),
            style = MaterialTheme.typography.titleMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            text = t("contacts.reauth.body"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
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
    maxUses = maxUses,
    fromSession = true,
)

private fun ActiveInvite.toRow(heldUrls: Map<String, String>): InviteLinkRow = InviteLinkRow(
    slug = blobIdSlug,
    // Sealed links: the server list carries no link secret. A link this device
    // created kept its secret (UX 5.3), so it is shareable; one created on
    // another device is not.
    url = heldUrls[blobIdSlug],
    label = label,
    createdAtMs = createdAtMs,
    expiryMs = expiryMs,
    maxUses = maxUses,
    fromSession = false,
)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun InviteLinksSheet(
    state: ContactsUiState,
    /** The synced record of each row by slug (UX section 9): the pencil and
     *  the confirmation before Delete. */
    linkSync: Map<String, InviteLinkSync>,
    onLoad: () -> Unit,
    onCreate: () -> Unit,
    onSaveEdit: (slug: String, InviteCreateParams, onSaved: () -> Unit) -> Unit,
    onCopy: (InviteLinkRow, String) -> Unit,
    onDelete: (InviteLinkRow) -> Unit,
    onDismiss: () -> Unit,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    // The server's list loads when the sheet opens; there is no Refresh button.
    LaunchedEffect(Unit) { onLoad() }
    val rows = remember(state.outstandingInvites, state.activeInvites, state.activeInviteUrls) {
        mergeInviteLinkRows(
            session = state.outstandingInvites.map { it.toRow() },
            server = state.activeInvites.map { it.toRow(state.activeInviteUrls) },
        )
    }
    val now = remember(rows) { System.currentTimeMillis() }
    val language = me.lakko.app.l10n.LocalStrings.current.language
    // The created fact carries the time too: words in the interface
    // language, order and clock from the device (localization.md 5).
    // The link whose terms are open (UX section 9), and the form's state.
    var editingSlug by remember { mutableStateOf<String?>(null) }
    var form by remember { mutableStateOf(InviteCreateForm()) }
    var qrUrl by remember { mutableStateOf<String?>(null) }
    var confirmDelete by remember { mutableStateOf<InviteLinkRow?>(null) }
    qrUrl?.let { url -> InviteQrSheet(url = url, onDismiss = { qrUrl = null }) }
    confirmDelete?.let { row ->
        // A Lakko dialog, never a system alert (UX section 9).
        me.lakko.app.ui.components.LakkoDialog(
            onDismissRequest = { confirmDelete = null },
            title = t("invite.links.delete_confirm_title"),
            body = t("invite.links.delete_confirm_body"),
            confirm = me.lakko.app.ui.components.LakkoDialogAction(
                t("common.delete"),
                me.lakko.app.ui.components.LakkoDialogTone.Danger,
            ) {
                confirmDelete = null
                onDelete(row)
            },
        )
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .imePadding()
                .padding(horizontal = 24.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(t("contacts.invite_links.title"), style = MaterialTheme.typography.titleLarge)
            Text(
                t("invite.links.desc"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            val error = state.inviteError ?: state.activeInvitesError
            if (error != null) {
                Text(error.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
            } else {
                state.inviteNotice?.let { notice ->
                    Text(notice.text(), color = MaterialTheme.colorScheme.primary, style = MaterialTheme.typography.bodySmall)
                }
            }
            val editing = editingSlug
            // While the form is open it takes the list's place, so the whole
            // form stays on screen above the keyboard.
            if (editing != null) {
                InviteCreateFormView(
                    form = form,
                    onChange = { form = it },
                    busy = state.generatingInvite,
                    onCreate = { params ->
                        onSaveEdit(editing, params) {
                            editingSlug = null
                            form = InviteCreateForm()
                        }
                    },
                    onCancel = {
                        editingSlug = null
                        form = InviteCreateForm()
                    },
                    modifier = Modifier.weight(1f, fill = false),
                    title = t("invite.links.edit_title"),
                    note = t("invite.links.edit_note"),
                    submitLabel = t("common.save"),
                    busyLabel = t("common.saving"),
                )
            } else if (state.loadingActiveInvites && rows.isEmpty()) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(8.dp))
                    Text(t("common.loading"), style = MaterialTheme.typography.bodySmall)
                }
            } else if (rows.isEmpty() && state.activeInvitesLoaded) {
                Text(
                    t("invite.links.empty"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (editing == null) LazyColumn(
                modifier = Modifier
                    .fillMaxWidth()
                    .weight(1f, fill = false),
            ) {
                items(rows, key = { it.slug }) { row ->
                    val meta = joinFacts(
                        buildList {
                            row.createdAtMs?.let {
                                add(t("invite.links.created", mapOf("date" to me.lakko.app.l10n.DeviceFormats.mediumDateTime(it, language))))
                            }
                            add(describeInviteExpiry(row.expiryMs, now).text())
                            add(describeInviteLimit(row.maxUses).text())
                        },
                    )
                    val share = inviteRowShare(row)
                    val shareUrl = (share as? InviteRowShare.Shareable)?.url
                    val sync = linkSync[row.slug]
                    val idle = state.revokingActiveInviteSlug == null && !state.generatingInvite
                    LinkRow(
                        title = inviteRowTitle(row),
                        meta = meta,
                        note = if (share is InviteRowShare.CreatedElsewhere) INVITE_CREATED_ELSEWHERE_COPY.text() else null,
                        onCopy = shareUrl?.let { url -> { onCopy(row, url) } },
                        onShowQr = shareUrl?.let { url -> { qrUrl = url } },
                        trailing = {
                            // The pencil only while nobody can hold the link.
                            if (inviteRowEditable(sync)) {
                                IconButton(
                                    onClick = {
                                        form = inviteFormFromTerms(
                                            label = row.label,
                                            expiryMs = row.expiryMs,
                                            mintedAtMs = row.createdAtMs,
                                            maxUses = row.maxUses,
                                        )
                                        editingSlug = row.slug
                                    },
                                    enabled = idle,
                                ) {
                                    Icon(Icons.Filled.Edit, contentDescription = t("invite.links.edit"))
                                }
                            }
                            if (state.revokingActiveInviteSlug == row.slug) {
                                Box(Modifier.size(48.dp), contentAlignment = Alignment.Center) {
                                    CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                                }
                            } else {
                                IconButton(
                                    onClick = {
                                        if (inviteDeleteNeedsConfirmation(sync)) confirmDelete = row else onDelete(row)
                                    },
                                    enabled = idle,
                                ) {
                                    Icon(
                                        Icons.Filled.Delete,
                                        contentDescription = t("invite.links.delete"),
                                        tint = if (idle) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                        },
                    )
                    HorizontalDivider()
                }
            }
            if (editing == null) {
                // Create link mints at once with the defaults (UX section 9).
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
                        Text(t("common.creating"))
                    } else {
                        Text(t("invite.links.create"))
                    }
                }
            }
        }
    }
}

/** A link row: title, meta line, a copy icon, an optional trailing action. The
 *  URL itself is never shown as text (UX 5.3). A link is handed out by Copy
 *  link, never by the system's share sheet (design system: no
 *  operating-system surface inside a Lakko screen). */
@Composable
private fun LinkRow(
    /** The label, or `null` for a link without one: the meta line leads. */
    title: String?,
    meta: String,
    onCopy: (() -> Unit)?,
    /** Opens the link's QR sheet; `null` when the row has no shareable URL. */
    onShowQr: (() -> Unit)? = null,
    /** One line shown under the meta line in place of Copy (UX 5.3: a link
     *  created on another device). */
    note: String? = null,
    trailing: @Composable () -> Unit = {},
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            if (title != null) {
                Text(title, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
            }
            if (meta.isNotEmpty()) {
                Text(
                    meta,
                    // With no title the meta line leads the row.
                    style = if (title == null) MaterialTheme.typography.bodyMedium else MaterialTheme.typography.bodySmall,
                    color = if (title == null) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (note != null) {
                Text(
                    note,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (onShowQr != null) {
            IconButton(onClick = onShowQr) {
                Icon(Icons.Filled.QrCode, contentDescription = t("invite.links.show_qr"))
            }
        }
        if (onCopy != null) {
            IconButton(onClick = onCopy) {
                Icon(Icons.Filled.ContentCopy, contentDescription = t("chat.invite_link.copy"))
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
    clipboard.setPrimaryClip(ClipData.newPlainText(me.lakko.app.l10n.Localization.strings.value["common.clipboard.link"], text))
}


