// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.Context
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.ScrollableTabRow
import androidx.compose.material3.TabRowDefaults
import androidx.compose.material3.TabRowDefaults.tabIndicatorOffset
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import me.lakko.app.chat.ChatFolder
import me.lakko.app.chat.ConversationListRow
import me.lakko.app.chat.folderRows
import me.lakko.app.chat.folderUnreadCount
import me.lakko.app.chat.hasUnread

/**
 * The folder bar, `docs/ux/chat-folders.md` section 2.
 *
 * A `ScrollableTabRow` under the app bar; the list under it is a
 * `HorizontalPager` in step. All chats is always first, carries no count and
 * cannot be edited. A folder's count is the conversations in it with anything
 * unread, not counting muted ones. Long press opens the tab's menu.
 */
@Composable
internal fun FolderBar(
    folders: List<ChatFolder>,
    rows: List<ConversationListRow>,
    selectedIndex: Int,
    onSelect: (Int) -> Unit,
    onEditFolder: (ChatFolder) -> Unit,
    onMarkAllRead: (List<ConversationListRow>) -> Unit,
    onReorder: () -> Unit,
    onDelete: (ChatFolder) -> Unit,
    onEditFolders: () -> Unit,
) {
    val counts = remember(folders, rows) { folders.map { folderUnreadCount(it, rows) } }
    val selected = selectedIndex.coerceIn(0, folders.size)
    ScrollableTabRow(
        selectedTabIndex = selected,
        edgePadding = 8.dp,
        divider = {},
        indicator = { positions ->
            if (selected < positions.size) {
                TabRowDefaults.SecondaryIndicator(
                    modifier = Modifier.tabIndicatorOffset(positions[selected]),
                    color = MaterialTheme.colorScheme.primary,
                )
            }
        },
    ) {
        FolderTab(
            label = "All chats",
            count = 0,
            selected = selected == 0,
            onClick = { onSelect(0) },
        ) { dismiss ->
            val unread = rows.filter { !it.membershipEnded && it.hasUnread }
            if (unread.isNotEmpty()) {
                DropdownMenuItem(
                    text = { Text("Mark all as read") },
                    onClick = { dismiss(); onMarkAllRead(unread) },
                )
            }
            DropdownMenuItem(
                text = { Text("Edit folders") },
                onClick = { dismiss(); onEditFolders() },
            )
        }
        folders.forEachIndexed { i, folder ->
            val count = counts.getOrElse(i) { 0 }
            FolderTab(
                label = folder.name,
                count = count,
                selected = selected == i + 1,
                onClick = { onSelect(i + 1) },
            ) { dismiss ->
                DropdownMenuItem(
                    text = { Text("Edit folder") },
                    onClick = { dismiss(); onEditFolder(folder) },
                )
                if (count > 0) {
                    DropdownMenuItem(
                        text = { Text("Mark all as read") },
                        onClick = {
                            dismiss()
                            onMarkAllRead(folderRows(folder, rows).filter { it.hasUnread })
                        },
                    )
                }
                DropdownMenuItem(
                    text = { Text("Reorder folders") },
                    onClick = { dismiss(); onReorder() },
                )
                DropdownMenuItem(
                    text = { Text("Delete folder", color = MaterialTheme.colorScheme.error) },
                    onClick = { dismiss(); onDelete(folder) },
                )
            }
        }
    }
}

/** One tab: the accent label with the accent underline when selected, muted
 *  otherwise; no pill, no icon (UX spec 2.1). */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun FolderTab(
    label: String,
    count: Int,
    selected: Boolean,
    onClick: () -> Unit,
    menuItems: @Composable (dismiss: () -> Unit) -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    val spoken = if (count > 0) {
        "$label, $count unread ${if (count == 1) "chat" else "chats"}"
    } else {
        label
    }
    val color = if (selected) {
        MaterialTheme.colorScheme.primary
    } else {
        MaterialTheme.colorScheme.onSurfaceVariant
    }
    Box {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .semantics(mergeDescendants = true) {
                    role = Role.Tab
                    this.selected = selected
                    contentDescription = spoken
                }
                .combinedClickable(onClick = onClick, onLongClick = { menuOpen = true })
                .padding(horizontal = 12.dp, vertical = 12.dp),
        ) {
            Text(
                text = label,
                style = MaterialTheme.typography.labelLarge,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Medium,
                color = color,
                maxLines = 1,
            )
            if (count > 0) {
                Spacer(Modifier.width(6.dp))
                Text(
                    text = if (count > 99) "99+" else count.toString(),
                    style = MaterialTheme.typography.labelMedium,
                    color = color,
                )
            }
        }
        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
            menuItems { menuOpen = false }
        }
    }
}

/** UX spec 2.4 — one line and one button, whatever the filter. */
@Composable
internal fun EmptyFolderPanel(onEdit: () -> Unit) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                text = "Nothing here right now",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            OutlinedButton(onClick = onEdit) { Text("Edit folder") }
        }
    }
}

/** UX spec 3.5. */
@Composable
internal fun DeleteFolderDialog(folder: ChatFolder, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Delete folder “${folder.name}”?") },
        text = { Text("The chats in it stay in All chats.") },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text("Delete", color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/**
 * The selected tab: a per-device fact, never synced (UX spec 2.5). A phone
 * left on "Family" does not flip the laptop to "Family". The folders it
 * names belong to the signed-in account, so the choice is kept per account
 * (`selected-folder.v2:<accountIdB64>`): a second account on the same phone
 * does not open on the first account's folder. With no account bound (see
 * [me.lakko.app.contacts.AcceptedContactsStore]) nothing is read or written.
 * The device-wide `selected-folder` key of earlier builds is never read.
 */
internal object FolderTabPrefs {
    private const val PREFS = "lakko-chat-folders"
    private const val KEY_PREFIX = "selected-folder.v2:"
    private const val RETIRED_KEY = "selected-folder"

    private fun key(accountIdB64: String): String =
        KEY_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

    private fun bound(): String? = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64

    fun load(context: Context): String? {
        val account = bound() ?: return null
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(key(account), null)
            ?.takeIf { it.isNotEmpty() }
    }

    fun save(context: Context, folderIdHex: String?) {
        val account = bound() ?: return
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putString(key(account), folderIdHex ?: "")
            .apply()
    }

    /** Forget device: drop [accountIdB64]'s choice and the retired key. */
    fun deleteAccount(context: Context, accountIdB64: String) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .remove(key(accountIdB64))
            .remove(RETIRED_KEY)
            .apply()
    }
}
