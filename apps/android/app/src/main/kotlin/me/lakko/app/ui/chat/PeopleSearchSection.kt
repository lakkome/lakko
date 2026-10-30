// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Campaign
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.search.OutgoingRequests
import me.lakko.app.search.PeopleSearchService
import me.lakko.app.search.PeopleSearchSession
import me.lakko.app.search.PeopleSearchState
import me.lakko.app.search.PersonResult
import me.lakko.app.search.RowAction
import me.lakko.app.search.RowSendState
import me.lakko.app.search.keptGrantsToCheck
import me.lakko.app.search.mergePeople
import me.lakko.app.search.peopleSectionMessage
import me.lakko.app.search.personRowView
import me.lakko.app.search.rowSendState
import me.lakko.app.ui.components.LakkoAvatar

/**
 * People under a search field (`docs/ux/who-can-find-me.md` section 6.1,
 * `docs/ux/navigation.md` section 4): the people this device already knows,
 * matched from the first letter, and what the network found for the text
 * under the heading **People**. Nothing to press: the [session] asks the
 * network on its own (3 characters, 500 ms after the last keystroke). The
 * same section sits under the Chats field and under the Contacts field.
 *
 * A row shows avatar, name and a verified `@username`, and never WHY the
 * person was found. Its control: a contact shows **Open**; someone who lets
 * this account find them shows **Add contact**, which sends the request at
 * once, with no introduction; anyone else shows their name only. A grant this
 * device merely kept is offered only after the Delivery Service answered for
 * it: until then the row is the name alone.
 */
@Composable
internal fun PeopleSearchSection(
    query: String,
    service: PeopleSearchService,
    session: PeopleSearchSession,
    /** How many rows the local filter above matched (conversations in Chats,
     *  contacts in Contacts). */
    conversationMatches: Int,
    onOpenChannel: (channelIdHex: String, handle: String) -> Unit,
    /** Open a contact's page (their chat). */
    onOpenContact: (keyB64: String) -> Unit,
    /** The line when nothing local matches; `null` for none. */
    noLocalMatchKey: String? = "chats.search.no_results",
    /** People already listed above (standard base64 account keys), left out. */
    excludeKeysB64: Set<String> = emptySet(),
) {
    val context = LocalContext.current
    val profileVersion by me.lakko.app.profile.ProfileDirectory.get(context).version.collectAsState()
    val contactsState by service.contactsState.collectAsState()
    val outgoing = remember(contactsState.sent) { OutgoingRequests.of(contactsState.sent) }
    LaunchedEffect(outgoing) { session.onOutgoing(outgoing) }
    val ownKeyHex = remember(profileVersion) { service.ownKeyHex() }
    val candidates = remember(query, profileVersion, contactsState.accepted) {
        if (query.isBlank()) emptyList() else runCatching { service.localCandidates() }.getOrDefault(emptyList())
    }
    val grantChecks by session.keptGrantChecks.collectAsState()
    val local = remember(query, candidates, grantChecks) {
        runCatching { service.local(query, candidates, grantChecks) }.getOrDefault(emptyList())
    }
    LaunchedEffect(query, candidates) {
        session.checkKeptGrants(keptGrantsToCheck(candidates, query, service.normalize, ownKeyHex))
    }
    val view by session.view.collectAsState()
    val sends by session.sendStates.collectAsState()
    val shown = view.shown.takeIf { view.asks }
    val people = remember(local, shown, ownKeyHex, excludeKeysB64) {
        mergePeople(listOf(local, shown?.people.orEmpty()), ownKeyHex).filter { it.keyB64 !in excludeKeysB64 }
    }
    val channel = shown?.channel
    val state = view.state
    val nothingLocal = conversationMatches == 0 && local.none { it.keyB64 !in excludeKeysB64 } && channel == null

    Column(modifier = Modifier.fillMaxWidth()) {
        if (noLocalMatchKey != null && nothingLocal && state != PeopleSearchState.FOUND) {
            Text(
                t(noLocalMatchKey),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            )
        }
        if (people.isNotEmpty() || channel != null || view.asks) {
            Heading(searching = view.searching)
        }
        for (person in people) {
            PersonRow(
                person = person,
                send = rowSendState(person, sends[person.keyHex], outgoing),
                onAdd = { session.add(person) },
                onOpenContact = onOpenContact,
            )
        }
        if (channel != null) {
            ChannelRow(
                label = t("search.people.channel_result", mapOf("handle" to "@${channel.handle}")),
                onClick = { onOpenChannel(channel.idHex, channel.handle) },
            )
        }
        peopleSectionMessage(state, people.size)?.let {
            Text(
                it.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
    }
}

@Composable
private fun Heading(searching: Boolean) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            t("search.people.heading"),
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (searching) {
            Spacer(Modifier.width(8.dp))
            val label = t("search.people.searching_a11y")
            CircularProgressIndicator(
                modifier = Modifier.size(14.dp).semantics { contentDescription = label },
                strokeWidth = 2.dp,
            )
        }
    }
}

@Composable
private fun ChannelRow(label: String, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Filled.Campaign, contentDescription = null)
        Spacer(Modifier.width(12.dp))
        Text(label, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
        Icon(Icons.Filled.ChevronRight, contentDescription = null)
    }
}

@Composable
private fun PersonRow(
    person: PersonResult,
    send: RowSendState?,
    onAdd: () -> Unit,
    onOpenContact: (String) -> Unit,
) {
    val row = personRowView(person, send)
    Column(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .then(
                    if (row.action == RowAction.OPEN) {
                        Modifier.clickable { onOpenContact(person.keyB64) }
                    } else {
                        Modifier
                    },
                )
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.Start,
        ) {
            LakkoAvatar(name = person.name, seed = person.keyB64, size = 40.dp, personKey = me.lakko.app.ui.components.personKeyBytes(person.keyB64))
            Spacer(Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(person.name, style = MaterialTheme.typography.bodyLarge)
                person.username?.let {
                    Text(
                        "@$it",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            when (row.action) {
                RowAction.OPEN ->
                    TextButton(onClick = { onOpenContact(person.keyB64) }) { Text(t("common.open")) }
                RowAction.ADD ->
                    TextButton(onClick = onAdd) { Text(t("search.people.add_contact")) }
                RowAction.ADDING ->
                    TextButton(onClick = {}, enabled = false) { Text(t("search.people.add_contact")) }
                RowAction.NONE -> Unit
            }
        }
        row.note?.let {
            Text(
                it.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(start = 68.dp, end = 16.dp, bottom = 4.dp),
            )
        }
    }
}
