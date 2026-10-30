// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
import androidx.compose.material3.InputChipDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.chat.AddCandidateContact
import me.lakko.app.chat.AddMemberPick
import me.lakko.app.chat.PendingRequestKind
import me.lakko.app.chat.addContactRows
import me.lakko.app.chat.addPersonRows
import me.lakko.app.chat.addSubmitLabel
import me.lakko.app.chat.outboxB64
import me.lakko.app.chat.pickOf
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.search.PeopleSearchService
import me.lakko.app.search.searchNormalizer
import me.lakko.app.search.searchStateMessage
import me.lakko.app.ui.components.LakkoAvatar

/**
 * Add members (`docs/ux/conversation-info.md` section 2.4): pushed over the
 * group's info, with the back arrow (and the system back) returning to it.
 *
 * A search field; the people picked, as removable chips; the contacts not in
 * the group and not already asked for, each with a checkbox, filtered on the
 * device from the first letter; and, from 3 characters, what the network
 * search finds under **People**, with a checkbox instead of Add contact. The
 * trailing **Add** (**Add {count}** from two) closes back to the info at once
 * and adds the picks there ([ChatViewModel.onAddPicked]).
 *
 * Who is listed and who can be picked is `me.lakko.app.chat.AddMembers`;
 * this screen renders its answers.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun AddMembersScreen(
    viewModel: ChatViewModel,
    contactsRepository: ContactsRepository,
    peopleSearch: PeopleSearchService?,
    modifier: Modifier = Modifier,
) {
    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    val members by viewModel.members.collectAsStateWithLifecycle()
    val pendingRequests by viewModel.pendingRequests.collectAsStateWithLifecycle()
    val requestedAddKeys by viewModel.requestedAddKeys.collectAsStateWithLifecycle()
    val picks by viewModel.addPicks.collectAsStateWithLifecycle()
    val busy by viewModel.membershipBusy.collectAsStateWithLifecycle()
    var query by rememberSaveable { mutableStateOf("") }

    val normalize = remember(peopleSearch) { peopleSearch?.normalize ?: searchNormalizer { null } }
    val scope = rememberCoroutineScope()
    val session = remember(peopleSearch, scope) { peopleSearch?.session(scope) }
    // A text kept across a rotation asks again from where it stood.
    LaunchedEffect(session) { session?.onQuery(query) }

    val memberKeys = remember(members) { members.map { outboxB64(it.signaturePublicKey) }.toSet() }
    val waitingKeys = remember(pendingRequests, requestedAddKeys) {
        pendingRequests
            .filter { it.kind == PendingRequestKind.ADD }
            .mapNotNull { it.addSignatureKeyB64 }
            .toSet() + requestedAddKeys
    }
    val candidates = remember(contactsState.accepted, nameVersion) {
        contactsState.accepted.map { c ->
            val resolved = directory.resolve(c.request.fromPubkey)
            AddCandidateContact(
                keyB64 = outboxB64(c.request.fromPubkey),
                accountIdB64 = c.request.fromAccountId?.takeIf { it.size == ACCOUNT_ID_LEN }?.let(::outboxB64),
                name = resolved.primary,
                username = resolved.verifiedUsername,
            )
        }
    }
    val contactRows = remember(candidates, memberKeys, waitingKeys, query) {
        addContactRows(candidates, memberKeys, waitingKeys, ownKeyB64 = null, query = query, normalize = normalize)
    }
    val networkView = session?.view?.collectAsState()?.value
    val shown = networkView?.shown?.takeIf { networkView.asks }
    val personRows = remember(shown, memberKeys, waitingKeys, contactRows) {
        addPersonRows(
            results = shown?.people.orEmpty(),
            memberKeysB64 = memberKeys,
            waitingKeysB64 = waitingKeys,
            listedKeysB64 = contactRows.map { it.contact.keyB64 }.toSet(),
            ownKeyB64 = null,
        )
    }
    val pickedKeys = picks.map { it.keyB64 }.toSet()

    BackHandler { viewModel.closeAddMembers() }
    Scaffold(
        modifier = modifier.fillMaxSize(),
        contentWindowInsets = WindowInsets.safeDrawing,
        topBar = {
            TopAppBar(
                title = { Text(t("chat.members.add.title")) },
                navigationIcon = {
                    IconButton(onClick = viewModel::closeAddMembers) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
                actions = {
                    TextButton(
                        onClick = viewModel::onAddPicked,
                        enabled = picks.isNotEmpty() && !busy,
                    ) { Text(addSubmitLabel(picks.size).text()) }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .imePadding(),
        ) {
            OutlinedTextField(
                value = query,
                onValueChange = {
                    query = it
                    session?.onQuery(it)
                },
                placeholder = { Text(t("chat.members.add.search_placeholder")) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            )
            if (picks.isNotEmpty()) {
                LazyRow(
                    contentPadding = PaddingValues(horizontal = 16.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    items(picks, key = { it.keyB64 }) { pick ->
                        PickChip(pick = pick, onRemove = { viewModel.toggleAddPick(pick) })
                    }
                }
            }
            LazyColumn(modifier = Modifier.weight(1f)) {
                items(contactRows, key = { "c:" + it.contact.keyB64 }) { row ->
                    val contact = row.contact
                    PickRow(
                        name = contact.name,
                        keyB64 = contact.keyB64,
                        line = if (row.pickable) contact.username?.let { "@$it" } else t("chats.new_group.accept_again"),
                        pickable = row.pickable,
                        checked = contact.keyB64 in pickedKeys,
                        onToggle = { pickOf(contact)?.let(viewModel::toggleAddPick) },
                    )
                }
                if (networkView != null && (networkView.asks || personRows.isNotEmpty())) {
                    item(key = "people-heading") { PeopleHeading(searching = networkView.searching) }
                    items(personRows, key = { "p:" + it.person.keyB64 }) { row ->
                        val person = row.person
                        PickRow(
                            name = person.name,
                            keyB64 = person.keyB64,
                            line = person.username?.let { "@$it" },
                            pickable = row.pickable,
                            checked = person.keyB64 in pickedKeys,
                            onToggle = { pickOf(person)?.let(viewModel::toggleAddPick) },
                        )
                    }
                    searchStateMessage(networkView.state)?.let { message ->
                        item(key = "people-state") {
                            Text(
                                message.text(),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun PickChip(pick: AddMemberPick, onRemove: () -> Unit) {
    val removeLabel = t("common.remove")
    InputChip(
        selected = true,
        onClick = onRemove,
        label = { Text(pick.name) },
        avatar = {
            LakkoAvatar(
                name = pick.name,
                seed = pick.keyB64,
                size = InputChipDefaults.AvatarSize,
                personKey = me.lakko.app.ui.components.personKeyBytes(pick.keyB64),
            )
        },
        trailingIcon = {
            Icon(
                Icons.Filled.Close,
                contentDescription = removeLabel,
                modifier = Modifier.size(InputChipDefaults.IconSize),
            )
        },
    )
}

@Composable
private fun PickRow(
    name: String,
    keyB64: String,
    line: String?,
    pickable: Boolean,
    checked: Boolean,
    onToggle: () -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .alpha(if (pickable) 1f else 0.5f)
            .clickable(enabled = pickable, onClick = onToggle)
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        LakkoAvatar(
            name = name,
            seed = keyB64,
            personKey = me.lakko.app.ui.components.personKeyBytes(keyB64),
        )
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(name, style = MaterialTheme.typography.titleMedium)
            line?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        Checkbox(checked = checked && pickable, onCheckedChange = null, enabled = pickable)
    }
}

/** **People**, with the spinner while the network is asked (the heading of
 *  `who-can-find-me.md` section 6.1, as the search fields draw it). */
@Composable
private fun PeopleHeading(searching: Boolean) {
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

private const val ACCOUNT_ID_LEN = 16
