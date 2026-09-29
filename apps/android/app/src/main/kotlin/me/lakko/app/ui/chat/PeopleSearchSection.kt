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
import androidx.compose.material.icons.filled.Groups
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.search.PeopleSearchOutcome
import me.lakko.app.search.PeopleSearchService
import me.lakko.app.search.PeopleSearchState
import me.lakko.app.search.PersonReach
import me.lakko.app.search.PersonResult
import me.lakko.app.search.REQUEST_REFUSED
import me.lakko.app.search.SendOutcome
import me.lakko.app.search.canAddContact
import me.lakko.app.search.mergePeople
import me.lakko.app.search.searchStateMessage
import me.lakko.app.ui.components.LakkoAvatar

/**
 * People in the Chats search field (`docs/ux/who-can-find-me.md` section 6.1,
 * `docs/ux/navigation.md` section 4): the people this device already knows,
 * matched as the person types, then one **Search people** row that asks the
 * network and adds what it finds to the same list. The Android twin of web
 * `PeopleSearch.tsx`.
 *
 * A row shows avatar, name and a verified `@username`, and never WHY the
 * person was found. Its action: a contact opens their chat; someone who lets
 * this account find them offers **Add contact**, which asks for an optional
 * introduction and sends the request with the key that found them; anyone
 * else shows their name only.
 */
@Composable
internal fun PeopleSearchSection(
    query: String,
    service: PeopleSearchService,
    /** How many conversations the local filter matched. */
    conversationMatches: Int,
    onOpenChannel: (channelIdHex: String, handle: String) -> Unit,
    /** Open a contact's page (their chat). */
    onOpenContact: (keyB64: String) -> Unit,
) {
    val context = LocalContext.current
    val profileVersion by me.lakko.app.profile.ProfileDirectory.get(context).version.collectAsState()
    val ownKeyHex = remember(profileVersion) { service.ownKeyHex() }
    val local = remember(query, profileVersion) {
        if (query.isBlank()) emptyList() else runCatching { service.local(query) }.getOrDefault(emptyList())
    }
    // The network half belongs to the query it was asked for.
    var network by remember { mutableStateOf<Pair<String, PeopleSearchOutcome?>?>(null) }
    val scope = rememberCoroutineScope()
    val state = network?.takeIf { it.first == query }?.let { it.second?.state ?: PeopleSearchState.SEARCHING }
        ?: PeopleSearchState.IDLE
    val found = network?.takeIf { it.first == query }?.second
    val people = remember(local, found, ownKeyHex) {
        mergePeople(listOf(local, found?.people.orEmpty()), ownKeyHex)
    }
    val channel = found?.channel
    val offersSearch = remember(query) { service.offersNetworkSearch(query) }
    val nothingLocal = conversationMatches == 0 && local.isEmpty() && channel == null

    Column(modifier = Modifier.fillMaxWidth()) {
        if (nothingLocal && state != PeopleSearchState.FOUND) {
            Text(
                "No results",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            )
        }
        if (people.isNotEmpty()) {
            Text(
                "People",
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
            for (person in people) {
                PersonRow(person = person, service = service, onOpenContact = onOpenContact)
            }
        }
        if (channel != null) {
            LookupRow(
                label = "Channel @${channel.handle}",
                icon = { Icon(Icons.Filled.Campaign, contentDescription = null) },
                busy = false,
                onClick = { onOpenChannel(channel.idHex, channel.handle) },
            )
        }
        if (offersSearch) {
            LookupRow(
                label = "Search people",
                icon = { Icon(Icons.Filled.Groups, contentDescription = null) },
                busy = state == PeopleSearchState.SEARCHING,
                onClick = {
                    if (state == PeopleSearchState.SEARCHING) return@LookupRow
                    val q = query
                    network = q to null
                    scope.launch {
                        val outcome = service.search(q)
                        if (network?.first == q) network = q to outcome
                    }
                },
            )
        }
        searchStateMessage(state)?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
    }
}

@Composable
private fun LookupRow(label: String, icon: @Composable () -> Unit, busy: Boolean, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = !busy, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        icon()
        Spacer(Modifier.width(12.dp))
        Text(label, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
        if (busy) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
        } else {
            Icon(Icons.Filled.ChevronRight, contentDescription = null)
        }
    }
}

@Composable
private fun PersonRow(person: PersonResult, service: PeopleSearchService, onOpenContact: (String) -> Unit) {
    var adding by remember(person.keyHex) { mutableStateOf(false) }
    var note by remember(person.keyHex) { mutableStateOf<String?>(null) }
    var sent by remember(person.keyHex) { mutableStateOf(false) }
    Column(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .then(
                    if (person.reach == PersonReach.CONTACT) {
                        Modifier.clickable { onOpenContact(person.keyB64) }
                    } else {
                        Modifier
                    },
                )
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.Start,
        ) {
            LakkoAvatar(name = person.name, seed = person.keyB64, size = 40.dp)
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
            if (canAddContact(person) && !sent) {
                TextButton(onClick = { adding = true }) { Text("Add contact") }
            }
        }
        note?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
    }
    if (adding) {
        AddContactDialog(
            person = person,
            service = service,
            onDone = { outcome ->
                when (outcome) {
                    SendOutcome.Sent -> {
                        sent = true
                        adding = false
                        note = "Request sent to ${person.name}. It waits under Sent until they accept."
                    }
                    SendOutcome.Refused -> note = REQUEST_REFUSED
                    is SendOutcome.Failed -> note = outcome.message
                }
            },
            onDismiss = { adding = false },
        )
    }
}

@Composable
private fun AddContactDialog(
    person: PersonResult,
    service: PeopleSearchService,
    onDone: (SendOutcome) -> Unit,
    onDismiss: () -> Unit,
) {
    var intro by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    AlertDialog(
        onDismissRequest = { if (!busy) onDismiss() },
        title = { Text("Add ${person.name}") },
        text = {
            Column {
                OutlinedTextField(
                    value = intro,
                    onValueChange = { intro = it },
                    label = { Text("Say who you are (optional)") },
                    minLines = 3,
                    maxLines = 6,
                    modifier = Modifier.fillMaxWidth(),
                )
                error?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                enabled = !busy,
                onClick = {
                    busy = true
                    error = null
                    scope.launch {
                        val outcome = service.send(person, intro)
                        busy = false
                        when (outcome) {
                            SendOutcome.Sent -> onDone(outcome)
                            SendOutcome.Refused -> error = REQUEST_REFUSED
                            is SendOutcome.Failed -> error = outcome.message
                        }
                    }
                },
            ) { Text(if (busy) "Sending..." else "Send") }
        },
        dismissButton = {
            TextButton(enabled = !busy, onClick = onDismiss) { Text("Cancel") }
        },
    )
}
