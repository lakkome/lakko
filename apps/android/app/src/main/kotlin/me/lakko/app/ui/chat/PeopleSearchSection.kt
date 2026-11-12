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
import me.lakko.app.search.channelResultLabel
import me.lakko.app.search.keptGrantsToCheck
import me.lakko.app.search.mergePeople
import me.lakko.app.search.peopleRows
import me.lakko.app.search.personRowView
import me.lakko.app.search.rowSendState
import me.lakko.app.search.searchEndLine
import me.lakko.app.search.searchShowsNothingFound
import me.lakko.app.search.withCurrentContacts
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

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
    noLocalMatchKey: String? = "search.nothing_found",
    /** People already listed above (standard base64 account keys), left out. */
    excludeKeysB64: Set<String> = emptySet(),
    /** The people whose one-to-one is among the conversations above (standard
     *  base64 account keys): a contact among them is left out. */
    chatPeersB64: Set<String> = emptySet(),
    /** The name of a channel this device knows, by its id; `null` when none. */
    channelName: (channelIdHex: String) -> String? = { null },
    /** ADR-0087 section 5 — public groups and channels under People. */
    conversations: ConversationSearchPort? = null,
) {
    val context = LocalContext.current
    val profileVersion by me.lakko.app.profile.ProfileDirectory.get(context).version.collectAsState()
    val contactsState by service.contactsState.collectAsState()
    val invited by service.invited.collectAsState()
    val outgoing = remember(contactsState.sent, invited) { OutgoingRequests.of(contactsState.sent, invited) }
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
    val contactKeys = remember(contactsState.accepted) {
        contactsState.accepted.mapTo(HashSet()) { java.util.Base64.getEncoder().encodeToString(it.request.fromPubkey) }
    }
    LaunchedEffect(contactKeys) { session.onContacts(contactKeys) }
    val people = remember(local, shown, ownKeyHex, excludeKeysB64, chatPeersB64, contactKeys) {
        val merged = mergePeople(listOf(local, shown?.people.orEmpty()), ownKeyHex)
        peopleRows(withCurrentContacts(merged, contactKeys), excludeKeysB64, chatPeersB64)
    }
    val channel = shown?.channel
    // ADR-0087 section 5 — the public groups and channels, asked here so the
    // field knows whether anything matched at all.
    val found = rememberFoundConversations(query, conversations)
    // who-can-find-me.md section 6.1, "One list": no headings. The rows this
    // device holds stand above (the caller's list), then the people (local
    // first), the groups and channels, the channel a handle names; the spinner
    // and the failure line at the end. "Nothing found" only when nothing
    // stands at all.
    val searching = view.searching || found.pending
    val endLine = if (view.searching) null else searchEndLine(view.state)
    val nothing = noLocalMatchKey != null && searchShowsNothingFound(
        listRows = conversationMatches,
        people = people.size,
        channelFound = channel != null,
        conversationsFound = found.rows.size,
        searching = searching,
        endLine = endLine != null,
    )

    Column(modifier = Modifier.fillMaxWidth()) {
        if (nothing && noLocalMatchKey != null) {
            Text(
                t(noLocalMatchKey),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            )
        }
        for (person in people) {
            PersonRow(
                person = person,
                send = rowSendState(person, sends[person.keyHex], outgoing),
                canAskAgain = outgoing.canAskAgain(person),
                onAdd = { session.add(person) },
                onAskAgain = { session.askAgain(person) },
                onOpenContact = onOpenContact,
            )
        }
        if (conversations != null) GroupsAndChannelsSection(found = found.rows, port = conversations)
        if (channel != null) {
            ChannelRow(
                label = channelResultLabel(channelName(channel.idHex), channel.handle),
                handle = channel.handle,
                onClick = { onOpenChannel(channel.idHex, channel.handle) },
            )
        }
        if (searching) SearchingRow()
        endLine?.let {
            Text(
                it.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
    }
}

/** The spinner at the end of the list while the network is asked. */
@Composable
private fun SearchingRow() {
    val label = t("search.people.searching_a11y")
    Row(
        modifier = Modifier.fillMaxWidth().padding(12.dp),
        horizontalArrangement = Arrangement.Center,
    ) {
        CircularProgressIndicator(
            modifier = Modifier.size(20.dp).semantics { contentDescription = label },
            strokeWidth = 2.dp,
        )
    }
}

@Composable
private fun ChannelRow(label: String, handle: String, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        LkIcon(R.drawable.ic_lk_megaphone, contentDescription = null, modifier = Modifier.size(40.dp).padding(8.dp))
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.bodyLarge)
            // Its second line says what it is.
            Text(
                t("search.people.channel_result", mapOf("handle" to "@$handle")),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        LkIcon(R.drawable.ic_lk_chevron_forward, contentDescription = null, size = LkIconSize.Mark)
    }
}

@Composable
private fun PersonRow(
    person: PersonResult,
    send: RowSendState?,
    canAskAgain: Boolean,
    onAdd: () -> Unit,
    onAskAgain: () -> Unit,
    onOpenContact: (String) -> Unit,
) {
    val row = personRowView(person, send, canAskAgain)
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
                RowAction.ASK_AGAIN ->
                    TextButton(onClick = onAskAgain) { Text(t("contacts.search.ask_again")) }
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
