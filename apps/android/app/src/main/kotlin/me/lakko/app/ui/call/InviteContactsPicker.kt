// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import me.lakko.app.calls.filterInviteContactRows
import me.lakko.app.calls.inviteContactsEmptyKey
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.chat.SearchField
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.contacts.ContactRowModel

/**
 * `Invite contacts` on the call screen (`docs/ux/call-links.md` section 3):
 * the person's contacts, a search field, and one press per contact.
 *
 * Contacts, not conversations: whom to invite is a question about people.
 * Where the invitation goes (the one-to-one with that contact, founded first
 * when there is none) is the caller's business, behind [onInvite].
 *
 * The list stays open so several contacts can be invited one after another;
 * a row in [invitedKeys] shows a check and `Invited` and cannot be pressed
 * twice. While [busyKey] is set nothing can be pressed and that row says it
 * is sending. [error] is one sentence above the list. Which rows there are
 * and how a query filters them is `calls/InviteContacts.kt`, where a test
 * can run it; [rows] arrive already ordered.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun InviteContactsPicker(
    rows: List<ContactRowModel>,
    busyKey: String?,
    invitedKeys: Set<String>,
    error: UiText?,
    onBack: () -> Unit,
    /** `docs/ux/who-can-find-me.md` 6.4 — rows whose contact request went
     *  inside a shared group (no invitation until they accept). */
    requestSentKeys: Set<String> = emptySet(),
    /** Rows nothing could be sent to (no grant, no shared group). */
    cannotMessageKeys: Set<String> = emptySet(),
    onInvite: (ContactRowModel) -> Unit,
    modifier: Modifier = Modifier,
    /** Catalog key of the title: what the pick does. The call screen's
     *  `Invite to call` by default; a channel passes its own. */
    titleKey: String = "call.invite.picker_title",
) {
    var query by rememberSaveable { mutableStateOf("") }
    val visible = remember(rows, query) { filterInviteContactRows(rows, query) }
    val busy = busyKey != null

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t(titleKey)) },
                navigationIcon = {
                    IconButton(onClick = onBack, enabled = !busy) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            // With no contacts there is nothing to search.
            if (rows.isNotEmpty()) SearchField(query = query, onQueryChange = { query = it })
            error?.let { message ->
                Text(
                    text = message.text(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
                )
            }
            val emptyKey = inviteContactsEmptyKey(contactCount = rows.size, visibleCount = visible.size)
            if (emptyKey != null) {
                Box(Modifier.fillMaxSize().padding(horizontal = 24.dp), contentAlignment = Alignment.Center) {
                    Text(
                        text = t(emptyKey),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            } else {
                LazyColumn(
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(vertical = 4.dp),
                ) {
                    items(items = visible, key = { it.key }) { row ->
                        InviteContactRow(
                            row = row,
                            sending = busyKey == row.key,
                            invited = row.key in invitedKeys,
                            requestSent = row.key in requestSentKeys,
                            cannotMessage = row.key in cannotMessageKeys,
                            enabled = !busy && row.key !in invitedKeys && row.key !in requestSentKeys,
                            onClick = { onInvite(row) },
                        )
                    }
                }
            }
        }
    }
}

/** The line under a row: sending, invited, a contact request sent inside a
 *  shared group (`docs/ux/who-can-find-me.md` 6.4; the request lines take the
 *  person's name), nothing sendable, or `null` for the username. */
internal fun inviteRowLineKey(sending: Boolean, invited: Boolean, requestSent: Boolean, cannotMessage: Boolean): String? = when {
    sending -> "common.sending"
    invited -> "call.invite.row_invited"
    requestSent -> "contacts.detail.request_sent"
    cannotMessage -> "contacts.detail.cannot_message"
    else -> null
}

/** One contact: the picture, the name, and under it what is happening to the
 *  invitation, else the `@username` when one is known. */
@Composable
private fun InviteContactRow(
    row: ContactRowModel,
    sending: Boolean,
    invited: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
    requestSent: Boolean = false,
    cannotMessage: Boolean = false,
) {
    val sendingLabel = t("common.sending")
    val invitedLabel = t("call.invite.row_invited")
    val line = inviteRowLineKey(sending = sending, invited = invited, requestSent = requestSent, cannotMessage = cannotMessage)
    val sub: String? = when (line) {
        null -> row.username?.takeIf { row.name != "@$it" }?.let { "@$it" }
        "common.sending" -> sendingLabel
        "call.invite.row_invited" -> invitedLabel
        else -> t(line, mapOf("name" to row.name))
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = enabled, onClick = onClick)
            .semantics(mergeDescendants = true) {
                if (sending) stateDescription = sendingLabel
                if (invited) stateDescription = invitedLabel
            }
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        LakkoAvatar(name = row.name, seed = row.key, size = 40.dp, personKey = me.lakko.app.ui.components.personKeyBytes(row.personKey ?: row.key))
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(row.name, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
            sub?.let {
                Text(
                    text = it,
                    style = MaterialTheme.typography.bodySmall,
                    color = if (invited) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (invited) {
            Icon(Icons.Filled.Check, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
        }
    }
}
