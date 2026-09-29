// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.reach.ReachApplyStatus
import me.lakko.app.reach.ReachManager
import me.lakko.app.reach.reachUnhex
import uniffi.lakko_ffi_uniffi.FfiGroupsForm
import uniffi.lakko_ffi_uniffi.FfiScopeId
import uniffi.lakko_ffi_uniffi.FfiScopeKind
import uniffi.lakko_ffi_uniffi.FfiVisibilityMode
import uniffi.lakko_ffi_uniffi.FfiVisibilitySetting

/**
 * `docs/ux/who-can-find-me.md` sections 3, 4, 8 and 9 — Settings > Privacy >
 * Who can find me.
 *
 * One single choice of three, two switches under the middle one, a single
 * choice of two under the second switch, and a list behind it. Every tap
 * takes effect at once (no Save button): the new setting is computed by the
 * Rust transition rules (`lakko_ui_rules`, UX section 11), handed to
 * [ReachManager.updateVisibility], and applied there — keys registered or
 * removed, the Everyone flag set, profiles republished.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun WhoCanFindMeScreen(
    reach: ReachManager,
    scopes: List<FindMeScope>,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val settingOrNull by reach.visibility.collectAsStateWithLifecycle()
    val status by reach.applyStatus.collectAsStateWithLifecycle()
    val coroutineScope = rememberCoroutineScope()
    var showList by rememberSaveable { mutableStateOf(false) }
    val setting = settingOrNull ?: reach.currentVisibility() ?: return

    fun apply(transform: (FfiVisibilitySetting) -> FfiVisibilitySetting) {
        val next = try {
            transform(setting)
        } catch (_: Throwable) {
            return
        }
        coroutineScope.launch { reach.updateVisibility(next) }
    }

    val model = setting.toModel()
    val rows = findMeRows(model, scopes)

    if (showList) {
        BackHandler { showList = false }
        ChosenScopesList(
            scopes = sortedFindMeScopes(scopes),
            chosen = model.chosen,
            onToggle = { scope, on -> apply { uniffi.lakko_ffi_uniffi.setScopeChosen(it, scope.toFfi(), on) } },
            onBack = { showList = false },
            modifier = modifier,
        )
        return
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text("Who can find me")
                        if (status == ReachApplyStatus.APPLYING) {
                            Spacer(Modifier.width(12.dp))
                            CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
                        }
                    }
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            item {
                findMeStatusLine(status)?.let { line ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                    ) {
                        Text(
                            line,
                            style = MaterialTheme.typography.bodyMedium,
                            color = if (status == ReachApplyStatus.FAILED) {
                                MaterialTheme.colorScheme.error
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                            modifier = Modifier.weight(1f),
                        )
                        if (status == ReachApplyStatus.FAILED) {
                            TextButton(onClick = { coroutineScope.launch { reach.retryApply() } }) {
                                Text("Retry")
                            }
                        }
                    }
                }
                Text(
                    FIND_ME_EXPLANATION,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
            }
            item {
                RadioRow(
                    title = "Nobody",
                    helper = FIND_ME_NOBODY_HELPER,
                    selected = model.mode == FindMeMode.NOBODY,
                    onSelect = { apply { uniffi.lakko_ffi_uniffi.chooseVisibilityMode(it, FfiVisibilityMode.NOBODY) } },
                )
                RadioRow(
                    title = "Some people",
                    helper = null,
                    selected = model.mode == FindMeMode.SOME,
                    onSelect = { apply { uniffi.lakko_ffi_uniffi.chooseVisibilityMode(it, FfiVisibilityMode.SOME) } },
                )
            }
            if (rows.showParts) {
                item {
                    SwitchRow(
                        title = "Contacts of my contacts",
                        helper = FIND_ME_FOF_HELPER,
                        checked = model.contactsOfContacts,
                        indent = 1,
                        onToggle = { on -> apply { uniffi.lakko_ffi_uniffi.setContactsOfContacts(it, on) } },
                    )
                    SwitchRow(
                        title = "Members of my groups and channels",
                        helper = rows.membersHelper,
                        checked = model.groupsForm != null,
                        indent = 1,
                        onToggle = { on -> apply { uniffi.lakko_ffi_uniffi.setGroupsVisible(it, on) } },
                    )
                }
            }
            if (rows.showForm) {
                item {
                    RadioRow(
                        title = "All groups and channels",
                        helper = null,
                        selected = model.groupsForm == FindMeForm.ALL,
                        indent = 2,
                        onSelect = { apply { uniffi.lakko_ffi_uniffi.setGroupsForm(it, FfiGroupsForm.ALL) } },
                    )
                    RadioRow(
                        title = "Only the ones I choose",
                        helper = null,
                        selected = model.groupsForm == FindMeForm.CHOSEN,
                        indent = 2,
                        trailingDisclosure = true,
                        onSelect = {
                            val firstTime = model.groupsForm != FindMeForm.CHOSEN && model.chosen.isEmpty()
                            apply { uniffi.lakko_ffi_uniffi.setGroupsForm(it, FfiGroupsForm.CHOSEN) }
                            // UX section 3 — selecting it for the first time
                            // opens the list straight away; the disclosure
                            // opens it whenever it is selected.
                            if (firstTime || model.groupsForm == FindMeForm.CHOSEN) showList = true
                        },
                    )
                }
            }
            item {
                RadioRow(
                    title = "Everyone",
                    helper = FIND_ME_EVERYONE_HELPER,
                    selected = model.mode == FindMeMode.EVERYONE,
                    onSelect = { apply { uniffi.lakko_ffi_uniffi.chooseVisibilityMode(it, FfiVisibilityMode.EVERYONE) } },
                )
            }
        }
    }
}

/** UX section 4 — one row per group and channel, groups then channels, each
 *  with a switch; a search field on top when there are many. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChosenScopesList(
    scopes: List<FindMeScope>,
    chosen: Set<String>,
    onToggle: (FindMeScope, Boolean) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var query by rememberSaveable { mutableStateOf("") }
    val shown = remember(scopes, query) {
        val q = query.trim().lowercase()
        if (q.isEmpty()) scopes else scopes.filter { it.name.lowercase().contains(q) }
    }
    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text("Only the ones I choose") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            if (scopes.isEmpty()) {
                item {
                    Text(
                        FIND_ME_NO_SCOPES,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(16.dp),
                    )
                }
                return@LazyColumn
            }
            if (scopes.size > SEARCH_THRESHOLD) {
                item {
                    OutlinedTextField(
                        value = query,
                        onValueChange = { query = it },
                        placeholder = { Text("Search") },
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = 16.dp, vertical = 8.dp),
                    )
                }
            }
            val groups = shown.filter { it.kind == FindMeScopeKind.GROUP }
            val channels = shown.filter { it.kind == FindMeScopeKind.CHANNEL }
            if (groups.isNotEmpty()) {
                item { SectionHeader("Groups") }
                items(groups, key = { it.chosenKey() }) { scope ->
                    SwitchRow(
                        title = scope.name,
                        helper = null,
                        checked = scope.chosenKey() in chosen,
                        indent = 0,
                        onToggle = { on -> onToggle(scope, on) },
                    )
                }
            }
            if (channels.isNotEmpty()) {
                item { SectionHeader("Channels") }
                items(channels, key = { it.chosenKey() }) { scope ->
                    SwitchRow(
                        title = scope.name,
                        helper = null,
                        checked = scope.chosenKey() in chosen,
                        indent = 0,
                        onToggle = { on -> onToggle(scope, on) },
                    )
                }
            }
        }
    }
}

@Composable
private fun SectionHeader(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun RadioRow(
    title: String,
    helper: String?,
    selected: Boolean,
    onSelect: () -> Unit,
    indent: Int = 0,
    trailingDisclosure: Boolean = false,
) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = helper?.let { { Text(it) } },
        leadingContent = { RadioButton(selected = selected, onClick = null) },
        trailingContent = if (trailingDisclosure) {
            { Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null) }
        } else {
            null
        },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier
            .padding(start = (indent * INDENT_DP).dp)
            .selectable(selected = selected, role = Role.RadioButton, onClick = onSelect),
    )
}

@Composable
private fun SwitchRow(
    title: String,
    helper: String?,
    checked: Boolean,
    indent: Int,
    onToggle: (Boolean) -> Unit,
) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = helper?.let { { Text(it) } },
        trailingContent = { Switch(checked = checked, onCheckedChange = null) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier
            .padding(start = (indent * INDENT_DP).dp)
            .toggleable(value = checked, role = Role.Switch, onValueChange = onToggle),
    )
}

/** The model the pure layer reads, from the FFI setting. */
internal fun FfiVisibilitySetting.toModel(): FindMeModel = FindMeModel(
    mode = when (mode) {
        FfiVisibilityMode.NOBODY -> FindMeMode.NOBODY
        FfiVisibilityMode.SOME -> FindMeMode.SOME
        FfiVisibilityMode.EVERYONE -> FindMeMode.EVERYONE
    },
    contactsOfContacts = contactsOfContacts,
    groupsForm = when (groups) {
        FfiGroupsForm.ALL -> FindMeForm.ALL
        FfiGroupsForm.CHOSEN -> FindMeForm.CHOSEN
        null -> null
    },
    chosen = chosen.map { scope ->
        val kind = if (scope.kind == FfiScopeKind.GROUP) FindMeScopeKind.GROUP else FindMeScopeKind.CHANNEL
        FindMeScope(kind, me.lakko.app.reach.reachHex(scope.id), "").chosenKey()
    }.toSet(),
)

/** A list row as the FFI's scope id. */
internal fun FindMeScope.toFfi(): FfiScopeId = FfiScopeId(
    kind = if (kind == FindMeScopeKind.GROUP) FfiScopeKind.GROUP else FfiScopeKind.CHANNEL,
    id = reachUnhex(idHex) ?: ByteArray(0),
)

/** More rows than fit a screen: show the search field (UX section 4). */
private const val SEARCH_THRESHOLD = 8

private const val INDENT_DP = 24

/**
 * `docs/ux/who-can-find-me.md` section 5 — the `Members can find me` switch in
 * group info and channel info, shown only while "Members of my groups and
 * channels" is on (absent otherwise: a switch that cannot be moved is noise).
 *
 * Under "Only the ones I choose" it is this scope's switch from the list.
 * Under "All" it shows on, and turning it off switches the setting to chosen
 * with every other group and channel on and this one off, saying so once.
 */
@Composable
public fun MembersCanFindMeRow(
    reach: ReachManager,
    kind: FindMeScopeKind,
    idHex: String,
    modifier: Modifier = Modifier,
) {
    val settingOrNull by reach.visibility.collectAsStateWithLifecycle()
    val setting = settingOrNull ?: return
    val model = setting.toModel()
    if (model.mode != FindMeMode.SOME || model.groupsForm == null) return
    val scope = FindMeScope(kind, idHex, "")
    val coroutineScope = rememberCoroutineScope()
    var notice by rememberSaveable(idHex) { mutableStateOf(false) }
    val on = when (model.groupsForm) {
        FindMeForm.ALL -> true
        FindMeForm.CHOSEN -> scope.chosenKey() in model.chosen
    }
    androidx.compose.foundation.layout.Column(modifier = modifier) {
        ListItem(
            headlineContent = { Text("Members can find me") },
            trailingContent = { Switch(checked = on, onCheckedChange = null) },
            colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
            modifier = Modifier.toggleable(value = on, role = Role.Switch) { wanted ->
                val next = try {
                    if (model.groupsForm == FindMeForm.ALL && !wanted) {
                        notice = true
                        uniffi.lakko_ffi_uniffi.excludeScopeUnderAll(setting, reach.knownScopes(), scope.toFfi())
                    } else {
                        uniffi.lakko_ffi_uniffi.setScopeChosen(setting, scope.toFfi(), wanted)
                    }
                } catch (_: Throwable) {
                    return@toggleable
                }
                coroutineScope.launch { reach.updateVisibility(next) }
            },
        )
        if (notice) {
            Text(
                MEMBERS_CAN_FIND_ME_NOTICE,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
        }
    }
}

/** UX section 5 — said once, in place, after a group is excluded under All. */
public const val MEMBERS_CAN_FIND_ME_NOTICE: String =
    "Now only the groups and channels you choose. Change in Privacy."
