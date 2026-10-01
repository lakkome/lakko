// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.chat.ChatFolder
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ConversationListRow
import me.lakko.app.chat.ExcludeFlag
import me.lakko.app.chat.FolderActions
import me.lakko.app.chat.FolderDraft
import me.lakko.app.chat.FolderLimits
import me.lakko.app.chat.IncludeKind
import me.lakko.app.chat.conversationRowMatchesQuery
import me.lakko.app.chat.orderConversationRows
import me.lakko.app.chat.refOf
import me.lakko.app.chat.rowIdOf
import me.lakko.app.chat.rowsMatching
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.l10n.t
import uniffi.lakko_ffi_uniffi.FolderConversationRef

/**
 * The folder editor, `docs/ux/chat-folders.md` section 3: the name, what is
 * included (three kind toggles, the chats added by hand, "Add chats"), what
 * is excluded (three toggles, the chats excluded by hand, "Exclude chats"),
 * and for an existing folder "Delete folder". Save is the top-bar action.
 *
 * Saving writes only the registers that changed. A chat the folder names that
 * this device does not hold is not shown here and is kept as it is - the
 * draft starts from every reference the folder holds, not from the rows this
 * device can draw.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun FolderEditorScreen(
    repository: ChatRepository,
    channels: ChannelsUiState?,
    deviceGroupSync: DeviceGroupSyncRepository,
    /** The folder to edit, or `null` for a new one. */
    folderIdHex: String?,
    /** "New folder..." from a row: the chat already added. */
    prefill: FolderConversationRef?,
    onDone: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    // Leaving must happen once: a delete both returns and makes the folder
    // disappear, and each would otherwise pop the screen.
    var finished by remember { mutableStateOf(false) }
    val finish = {
        if (!finished) {
            finished = true
            onDone()
        }
    }
    val folderViews by deviceGroupSync.folders.collectAsStateWithLifecycle()
    val folders = remember(folderViews) { folderViews.map(ChatFolder::of) }
    val original = remember(folders, folderIdHex) {
        folderIdHex?.let { id -> folders.firstOrNull { it.idHex == id } }
    }
    // Deleted on another device while open: nothing left to edit.
    var seenOriginal by remember { mutableStateOf(false) }
    if (original != null) seenOriginal = true
    if (folderIdHex != null && original == null && seenOriginal) {
        LaunchedEffect(Unit) { finish() }
    }
    val actions = remember(deviceGroupSync, repository) { FolderActions(deviceGroupSync, repository) }
    val allRows = rememberConversationRows(repository, channels, deviceGroupSync)
    val pickable = remember(allRows) { orderConversationRows(allRows).filter { !it.membershipEnded } }

    val initial = remember(original?.idHex) {
        original?.let(FolderDraft::of) ?: FolderDraft.empty(prefill)
    }
    var draft by remember(original?.idHex) { mutableStateOf(initial) }
    var confirmingDiscard by remember { mutableStateOf(false) }
    var confirmingDelete by remember { mutableStateOf(false) }
    var picking by remember { mutableStateOf<FolderPickMode?>(null) }
    var saving by remember { mutableStateOf(false) }

    val dirty = !draft.sameAs(initial)
    val nameValid = remember(draft.name) { isValidFolderNameSafely(draft.name.trim()) }
    val atFolderLimit = original == null && folders.size >= FolderLimits.MAX_FOLDERS
    val canSave = draft.canSave && nameValid && !atFolderLimit && !saving
    val hasRule = draft.includeKinds != 0u || draft.included.isNotEmpty()
    val matchCount = remember(draft, allRows) { rowsMatching(draft.rule, allRows).size }

    val leave = { if (dirty) confirmingDiscard = true else finish() }
    BackHandler(onBack = leave)

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (original == null) t("folders.new") else t("folders.edit")) },
                navigationIcon = {
                    IconButton(onClick = leave) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
                actions = {
                    TextButton(
                        enabled = canSave,
                        onClick = {
                            saving = true
                            scope.launch {
                                val ok = actions.save(original, draft)
                                saving = false
                                if (ok) finish()
                            }
                        },
                    ) { Text(t("common.save")) }
                },
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
            contentPadding = PaddingValues(bottom = 24.dp),
        ) {
            item {
                OutlinedTextField(
                    value = draft.name,
                    // The field stops accepting input at the limit; no error
                    // text (UX spec 3.4).
                    onValueChange = { next ->
                        val clean = next.replace("\n", "").replace("\r", "")
                        if (clean.codePointCount(0, clean.length) <= FolderLimits.MAX_NAME) {
                            draft = draft.copy(name = clean)
                        }
                    },
                    placeholder = { Text(t("folders.editor.name_placeholder")) },
                    singleLine = true,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 12.dp),
                )
                if (atFolderLimit) {
                    HelperLine(t("folders.limit", emptyMap(), mapOf("count" to FolderLimits.MAX_FOLDERS.toULong())))
                }
            }

            item { SectionHeader(t("folders.editor.included")) }
            if (hasRule) {
                item { HelperLine(chatsCount(matchCount)) }
            }
            item {
                KindToggle(t("folders.editor.kind.one_to_one"), draft.includeKinds and IncludeKind.ONE_TO_ONE != 0u) {
                    draft = draft.copy(includeKinds = draft.includeKinds xor IncludeKind.ONE_TO_ONE)
                }
                KindToggle(t("folders.editor.kind.groups"), draft.includeKinds and IncludeKind.GROUPS != 0u) {
                    draft = draft.copy(includeKinds = draft.includeKinds xor IncludeKind.GROUPS)
                }
                KindToggle(t("folders.editor.kind.channels"), draft.includeKinds and IncludeKind.CHANNELS != 0u) {
                    draft = draft.copy(includeKinds = draft.includeKinds xor IncludeKind.CHANNELS)
                }
            }
            items(items = resolvedRows(draft.included, pickable), key = { "in:${it.id}" }) { row ->
                HandRow(row = row) {
                    draft = draft.copy(included = draft.included.filter { rowIdOf(it) != row.id })
                }
            }
            item { ActionRow(t("folders.editor.add_chats")) { picking = FolderPickMode.Include } }

            item { SectionHeader(t("folders.editor.excluded")) }
            item {
                KindToggle(t("folders.editor.exclude.muted"), draft.excludeFlags and ExcludeFlag.MUTED != 0u) {
                    draft = draft.copy(excludeFlags = draft.excludeFlags xor ExcludeFlag.MUTED)
                }
                KindToggle(t("folders.editor.exclude.read"), draft.excludeFlags and ExcludeFlag.READ != 0u) {
                    draft = draft.copy(excludeFlags = draft.excludeFlags xor ExcludeFlag.READ)
                }
                KindToggle(t("folders.editor.exclude.archived"), draft.excludeFlags and ExcludeFlag.ARCHIVED != 0u) {
                    draft = draft.copy(excludeFlags = draft.excludeFlags xor ExcludeFlag.ARCHIVED)
                }
            }
            items(items = resolvedRows(draft.excluded, pickable), key = { "out:${it.id}" }) { row ->
                HandRow(row = row) {
                    draft = draft.copy(excluded = draft.excluded.filter { rowIdOf(it) != row.id })
                }
            }
            item { ActionRow(t("folders.editor.exclude_chats")) { picking = FolderPickMode.Exclude } }

            if (original != null) {
                item {
                    Spacer(Modifier.height(24.dp))
                    HorizontalDivider()
                    Text(
                        t("folders.delete"),
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodyLarge,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable { confirmingDelete = true }
                            .padding(horizontal = 16.dp, vertical = 16.dp),
                    )
                }
            }
        }
    }

    picking?.let { mode ->
        val current = if (mode == FolderPickMode.Include) draft.included else draft.excluded
        ChatPicker(
            title = if (mode == FolderPickMode.Include) t("folders.editor.add_chats") else t("folders.editor.exclude_chats"),
            limitLine = if (mode == FolderPickMode.Include) {
                t("folders.picker.limit_include", emptyMap(), mapOf("count" to FolderLimits.MAX_HAND.toULong()))
            } else {
                t("folders.picker.limit_exclude", emptyMap(), mapOf("count" to FolderLimits.MAX_HAND.toULong()))
            },
            rows = pickable,
            initiallyChecked = current.map(::rowIdOf).toSet(),
            // References this device cannot draw still count toward the limit.
            unresolvedCount = current.count { ref -> pickable.none { it.id == rowIdOf(ref) } },
            onDismiss = { picking = null },
            onDone = { checkedIds ->
                picking = null
                draft = applyPick(draft, mode, checkedIds, pickable)
            },
        )
    }

    if (confirmingDiscard) {
        AlertDialog(
            onDismissRequest = { confirmingDiscard = false },
            title = { Text(t("folders.editor.discard.title")) },
            confirmButton = {
                TextButton(onClick = { confirmingDiscard = false; finish() }) { Text(t("folders.editor.discard.confirm")) }
            },
            dismissButton = {
                TextButton(onClick = { confirmingDiscard = false }) { Text(t("folders.editor.discard.keep")) }
            },
        )
    }
    if (confirmingDelete && original != null) {
        DeleteFolderDialog(
            folder = original,
            onDismiss = { confirmingDelete = false },
            onConfirm = {
                confirmingDelete = false
                scope.launch {
                    if (actions.delete(original)) finish()
                }
            },
        )
    }
}

internal enum class FolderPickMode { Include, Exclude }

/**
 * The picker's result applied to the draft: the checked rows become the set,
 * references this device cannot draw are kept, and a chat moved into one set
 * leaves the other.
 */
internal fun applyPick(
    draft: FolderDraft,
    mode: FolderPickMode,
    checkedIds: Set<String>,
    rows: List<ConversationListRow>,
): FolderDraft {
    val visibleIds = rows.map { it.id }.toSet()
    val current = if (mode == FolderPickMode.Include) draft.included else draft.excluded
    val unresolved = current.filter { rowIdOf(it) !in visibleIds }
    val kept = current.filter { rowIdOf(it) in checkedIds }
    val keptIds = kept.map(::rowIdOf).toSet()
    val added = rows.filter { it.id in checkedIds && it.id !in keptIds }.mapNotNull(::refOf)
    val next = unresolved + kept + added
    val nextIds = next.map(::rowIdOf).toSet()
    return if (mode == FolderPickMode.Include) {
        draft.copy(included = next, excluded = draft.excluded.filter { rowIdOf(it) !in nextIds })
    } else {
        draft.copy(excluded = next, included = draft.included.filter { rowIdOf(it) !in nextIds })
    }
}

/** The rows a set of references names that this device can draw, in set order. */
private fun resolvedRows(refs: List<FolderConversationRef>, rows: List<ConversationListRow>): List<ConversationListRow> {
    val byId = rows.associateBy { it.id }
    return refs.mapNotNull { byId[rowIdOf(it)] }.distinctBy { it.id }
}

/** "3 chats": how many conversations a folder's rule matches. */
@Composable
internal fun chatsCount(n: Int): String = t("folders.chat_count", emptyMap(), mapOf("count" to n.coerceAtLeast(0).toULong()))

private fun isValidFolderNameSafely(name: String): Boolean = try {
    uniffi.lakko_ffi_uniffi.isValidFolderName(name)
} catch (_: Throwable) {
    name.isNotEmpty()
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
private fun HelperLine(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
    )
}

/** A real checkable row (UX spec 9), not tinted text. */
@Composable
private fun KindToggle(label: String, checked: Boolean, onToggle: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .toggleable(value = checked, role = Role.Checkbox, onValueChange = { onToggle() })
            .padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Text(label, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
        Checkbox(checked = checked, onCheckedChange = null)
    }
}

@Composable
private fun HandRow(row: ConversationListRow, onRemove: () -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        ConversationRowBody(row = row, modifier = Modifier.weight(1f))
        IconButton(onClick = onRemove) {
            Icon(Icons.Filled.Close, contentDescription = t("folders.editor.remove_chat_a11y", mapOf("name" to row.title)))
        }
        Spacer(Modifier.width(4.dp))
    }
}

@Composable
private fun ActionRow(label: String, onClick: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
    ) {
        Icon(Icons.Filled.Add, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
        Spacer(Modifier.width(16.dp))
        Text(label, color = MaterialTheme.colorScheme.primary, style = MaterialTheme.typography.bodyLarge)
    }
}

/**
 * The shared chat picker, UX spec 3.3: full screen, the search field on top,
 * the merged list with a checkbox per row, "Done" to return. The same picker
 * serves "Add chats" and "Exclude chats"; its title says which.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChatPicker(
    title: String,
    limitLine: String,
    rows: List<ConversationListRow>,
    initiallyChecked: Set<String>,
    unresolvedCount: Int,
    onDismiss: () -> Unit,
    onDone: (Set<String>) -> Unit,
) {
    var checked by remember { mutableStateOf(initiallyChecked) }
    var query by remember { mutableStateOf("") }
    val visible = remember(rows, query) {
        rows.filter { conversationRowMatchesQuery(query, it.title, it.preview) }
    }
    val full = checked.size + unresolvedCount >= FolderLimits.MAX_HAND
    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(title) },
                    navigationIcon = {
                        IconButton(onClick = onDismiss) {
                            Icon(Icons.Filled.Close, contentDescription = t("common.close"))
                        }
                    },
                    actions = {
                        TextButton(onClick = { onDone(checked) }) { Text(t("common.done")) }
                    },
                )
            },
        ) { padding ->
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
            ) {
                SearchField(query = query, onQueryChange = { query = it })
                if (full) HelperLine(limitLine)
                LazyColumn(modifier = Modifier.fillMaxSize()) {
                    items(items = visible, key = { "${it.kind}:${it.id}" }) { row ->
                        val isChecked = row.id in checked
                        val enabled = isChecked || !full
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier.toggleable(
                                value = isChecked,
                                enabled = enabled,
                                role = Role.Checkbox,
                                onValueChange = { on ->
                                    checked = if (on) checked + row.id else checked - row.id
                                },
                            ),
                        ) {
                            ConversationRowBody(row = row, modifier = Modifier.weight(1f))
                            Checkbox(checked = isChecked, onCheckedChange = null, enabled = enabled)
                            Spacer(Modifier.width(8.dp))
                        }
                    }
                }
            }
        }
    }
}
