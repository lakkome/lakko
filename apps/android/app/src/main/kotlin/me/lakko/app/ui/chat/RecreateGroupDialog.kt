// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import uniffi.lakko_ffi_uniffi.GroupMember
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.contacts.ResetRecord
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.joinList
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.components.LakkoAvatar

/**
 * ADR-0065 phase 2 / `docs/ux/identity-recovery.md` 16.5 — `Recreate group`:
 * the new-group flow of `docs/ux/new-group.md` pre-filled, hosted the way the
 * new-group flow is on this platform (a full-width dialog).
 *
 * Pre-filled with every current member except the old owner; each may be
 * unticked. The new identity — the reset contact — is listed unticked and
 * disabled with the two-line reason while `reset-unverified` on this device,
 * ticked by default and enabled once verified. The sheet does not offer to
 * verify from here; the contact screen does that. Other admins become plain
 * members of the new group (the sheet says nothing about roles).
 *
 * `Create and close the old group` is one action: create, invite every ticked
 * member through the ordinary add path, post the closed notice into the OLD
 * group naming the new one, open the new group. While it works the sheet
 * cannot be dismissed; when some invitations failed it stays with the names
 * and a `Done` button, the new group open behind it.
 *
 * A member of the old group who is not a contact on this device cannot be
 * invited (the add path needs an account id); such a row is listed dimmed,
 * unticked, like a pre-ADR-0021 contact in the new-group flow.
 */
@Composable
internal fun RecreateGroupDialog(
    viewModel: ChatViewModel,
    contactsRepository: ContactsRepository,
    oldOwnerRecord: ResetRecord,
    members: List<GroupMember>,
    onDismiss: () -> Unit,
    onCreated: (groupIdHex: String) -> Unit,
) {
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    val resetRecords by contactsRepository.resetRecords.collectAsStateWithLifecycle()
    val recreating by viewModel.recreating.collectAsStateWithLifecycle()
    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val ownLeaf = viewModel.ownLeafIndex()

    // The rows: every member except this device and the old owner's old key,
    // each resolved to the contact that can be invited (by account key, or by
    // a reset record's keys so the new identity lands on its contact row).
    val rows = remember(members, contactsState.accepted, resetRecords, nameVersion) {
        members
            .filter { it.leafIndex != ownLeaf }
            .mapNotNull { member ->
                val keyB64 = me.lakko.app.chat.outboxB64(member.signaturePublicKey)
                if (keyB64 == oldOwnerRecord.previousKeyB64) return@mapNotNull null
                val contact = contactsState.accepted.firstOrNull {
                    it.request.fromPubkey.contentEquals(member.signaturePublicKey)
                } ?: contactsRepository.contactByRouteKey(
                    me.lakko.app.ui.contacts.contactRouteKey(member.signaturePublicKey),
                )
                val record = contact?.request?.fromAccountId
                    ?.let { resetRecords[me.lakko.app.auth.base64Encode(it)] }
                RecreateRow(
                    memberKeyB64 = keyB64,
                    label = viewModel.memberLabel(member),
                    contact = contact?.takeIf { it.request.fromAccountId != null },
                    resetUnverified = record?.unverified == true,
                )
            }
    }
    var name by remember { mutableStateOf(viewModel.groupName.value) }
    val picked = remember(rows) {
        mutableStateOf(rows.filter { it.contact != null && !it.resetUnverified }.map { it.memberKeyB64 }.toSet())
    }
    var error by remember { mutableStateOf<UiText?>(null) }
    var someFailed by remember { mutableStateOf<List<String>?>(null) }
    var createdHex by remember { mutableStateOf<String?>(null) }

    // The new group was founded and its closed notice is still owed (this
    // press, or an earlier one before the sheet was reopened): the name and
    // the member list are settled, and the button only posts the notice.
    var closeOwed by remember {
        mutableStateOf(viewModel.recreateStep() is me.lakko.app.chat.RecreateStep.RepostClose)
    }
    // Invitations that failed on the press that created the group, shown once
    // the notice has gone out.
    var owedFailedNames by remember { mutableStateOf<List<String>?>(null) }

    Dialog(
        onDismissRequest = { if (!recreating) onDismiss() },
        properties = DialogProperties(
            usePlatformDefaultWidth = false,
            dismissOnBackPress = !recreating,
            dismissOnClickOutside = !recreating,
        ),
    ) {
        Surface(
            modifier = Modifier
                .fillMaxWidth()
                .padding(16.dp),
            shape = MaterialTheme.shapes.large,
        ) {
            Column(modifier = Modifier.padding(16.dp)) {
                Text(t("group.recreate.title"), style = MaterialTheme.typography.titleMedium)
                Spacer(Modifier.height(12.dp))
                OutlinedTextField(
                    value = name,
                    onValueChange = { if (it.length <= GroupPresentationStore.MAX_NAME_CHARS) name = it },
                    label = { Text(t("chats.new_group.name_label")) },
                    singleLine = true,
                    enabled = !recreating && someFailed == null && !closeOwed,
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(12.dp))
                Text(
                    t("group.recreate.members_heading", emptyMap(), mapOf("count" to rows.size.toULong())),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
                LazyColumn(modifier = Modifier.heightIn(max = 280.dp)) {
                    items(rows, key = { it.memberKeyB64 }) { row ->
                        val canPick = row.contact != null && !row.resetUnverified && someFailed == null && !closeOwed
                        val checked = row.memberKeyB64 in picked.value
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier
                                .fillMaxWidth()
                                .alpha(if (canPick) 1f else 0.5f)
                                .clickable(enabled = canPick && !recreating) {
                                    picked.value = if (checked) picked.value - row.memberKeyB64 else picked.value + row.memberKeyB64
                                }
                                .padding(vertical = 6.dp),
                        ) {
                            LakkoAvatar(name = row.label, seed = row.memberKeyB64, size = 32.dp)
                            Spacer(Modifier.width(12.dp))
                            Column(modifier = Modifier.weight(1f)) {
                                Text(row.label, style = MaterialTheme.typography.bodyLarge)
                                // UX 16.5 — the new identity, unticked and
                                // disabled with the two-line reason.
                                if (row.resetUnverified) {
                                    Text(
                                        t("group.recreate.excluded_reset"),
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                } else if (row.contact == null) {
                                    Text(
                                        t("chats.new_group.accept_again"),
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                            Checkbox(checked = checked, onCheckedChange = null, enabled = canPick && !recreating)
                        }
                    }
                }
                Spacer(Modifier.height(8.dp))
                Text(
                    t("group.recreate.note"),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                error?.let {
                    Spacer(Modifier.height(6.dp))
                    Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                }
                someFailed?.let { names ->
                    Spacer(Modifier.height(6.dp))
                    Text(
                        t("chat.members.add.error.some_failed", mapOf("names" to joinList(names))),
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Spacer(Modifier.height(12.dp))
                Row(horizontalArrangement = androidx.compose.foundation.layout.Arrangement.End, modifier = Modifier.fillMaxWidth()) {
                    if (someFailed != null) {
                        // Created, some invitations failed: the group exists and
                        // is open behind the sheet; `Done` leaves it there.
                        Button(onClick = { createdHex?.let(onCreated) ?: onDismiss() }) {
                            Text(t("group.recreate.done"))
                        }
                    } else {
                        TextButton(onClick = onDismiss, enabled = !recreating) { Text(t("common.cancel")) }
                        Spacer(Modifier.width(8.dp))
                        Button(
                            enabled = !recreating && (closeOwed || picked.value.isNotEmpty()),
                            onClick = {
                                val chosen: List<AcceptedContact> = rows
                                    .filter { it.memberKeyB64 in picked.value }
                                    .mapNotNull { it.contact }
                                // A notice-only retry invites nobody, so it
                                // needs nobody ticked.
                                if (chosen.isEmpty() && !closeOwed) return@Button
                                error = null
                                scope.launch {
                                    val outcome = try {
                                        viewModel.recreateGroup(name, chosen)
                                    } catch (cause: Throwable) {
                                        android.util.Log.w("RecreateGroupDialog", "recreate failed", cause)
                                        // UX 16.5 — offline reads as the connection,
                                        // anything else as the new-group failure copy.
                                        error = me.lakko.app.chat.anonTokenFailureText(
                                            cause, me.lakko.app.chat.AllowanceKind.ACTION,
                                        ) ?: if (cause is java.io.IOException) {
                                            UiText("common.error.offline")
                                        } else {
                                            UiText("chats.new_group.error")
                                        }
                                        return@launch
                                    }
                                    createdHex = outcome.newGroupIdHex
                                    if (!outcome.closedOld) {
                                        // The new group exists but the closed
                                        // notice did not reach the old one. The
                                        // sheet stays; the same button now only
                                        // posts the notice again, naming the
                                        // same group, and never creates a second.
                                        error = outcome.closeFailure?.let {
                                            me.lakko.app.chat.anonTokenFailureText(it, me.lakko.app.chat.AllowanceKind.ACTION)
                                        } ?: if (outcome.closeFailure is java.io.IOException) {
                                            UiText("common.error.offline")
                                        } else {
                                            UiText("chat.send.failed")
                                        }
                                        closeOwed = true
                                        if (outcome.failed.isNotEmpty()) {
                                            owedFailedNames = outcome.failed.map { c ->
                                                directory.resolve(c.request.fromPubkey).primary
                                            }
                                        }
                                        return@launch
                                    }
                                    closeOwed = false
                                    val failedNames = owedFailedNames
                                    owedFailedNames = null
                                    if (failedNames != null) {
                                        someFailed = failedNames
                                    } else if (outcome.failed.isEmpty()) {
                                        onCreated(outcome.newGroupIdHex)
                                    } else {
                                        someFailed = outcome.failed.map { c ->
                                            directory.resolve(c.request.fromPubkey).primary
                                        }
                                    }
                                }
                            },
                        ) {
                            Text(t(if (recreating) "group.recreate.working" else "group.recreate.submit"))
                        }
                    }
                }
            }
        }
    }
}

/** One row of the recreate sheet. */
private data class RecreateRow(
    val memberKeyB64: String,
    val label: String,
    /** The contact the invitation goes to; `null` when none can be invited. */
    val contact: AcceptedContact?,
    /** UX 16.5 — the new identity while `reset-unverified` on this device. */
    val resetUnverified: Boolean,
)
