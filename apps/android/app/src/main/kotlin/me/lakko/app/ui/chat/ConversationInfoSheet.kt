// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Chat
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.contacts.ActionButton
import uniffi.lakko_ffi_uniffi.InfoSection
import uniffi.lakko_ffi_uniffi.PersonInfoCapabilities
import uniffi.lakko_ffi_uniffi.personInfoSections

/**
 * The fallback for a one-to-one whose other person this device holds no
 * contact record for — docs/ux/conversation-info.md section 2.1, "kept only
 * as a fallback for a roster that cannot be read".
 *
 * This is deliberately the ONLY person surface left in this file. A person
 * who IS a contact has a screen of their own, [me.lakko.app.ui.contacts.ContactDetailScreen],
 * and the header pushes it exactly as the contacts list does (section 3,
 * "Except a person"): until 2026-09-26 the header hosted that screen inside
 * this sheet, without `Message`, and a person looking at the two saw two cards
 * for one contact. The group's layer is `MembersSheet`.
 *
 * A layer, not a panel (section 3): the conversation stays where it is behind
 * it, and the sheet's handle and its scrim are the one obvious way out.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun UnknownPersonSheet(
    /** The name the chat header shows. */
    peerName: String,
    /** Their verified username, without the "@", or `null`. */
    peerUsername: String?,
    avatarSeed: String,
    /** Start a call on this conversation. `null` while a call already holds it. */
    onStartCall: ((video: Boolean) -> Unit)?,
    /** ADR-0045 UX 6.2.4 — forget a conversation this device is out of. */
    onDeleteChat: () -> Unit,
    /** Has this device's own membership in the conversation ended? */
    membershipEnded: Boolean,
    onDismiss: () -> Unit,
) {
    // Section 2.1 / section 4 — what this holds is asked, never decided here:
    // `crates/lakko-ui-rules/src/info.rs` is the one copy of that rule.
    // Remembered because asking now crosses the FFI: the rule is a native
    // call, and its answer changes only when one of its inputs does.
    val sections = remember(membershipEnded) {
        personInfoSections(
            PersonInfoCapabilities(isContact = false, membershipEnded = membershipEnded),
        )
    }
    val callAction = onStartCall?.takeIf { InfoSection.CALL in sections }
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    var confirmingDelete by remember { mutableStateOf(false) }
    if (confirmingDelete) {
        DeleteChatDialog(
            onCancel = { confirmingDelete = false },
            onDelete = {
                confirmingDelete = false
                onDeleteChat()
            },
        )
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        UnknownPersonBody(
            name = peerName,
            username = peerUsername,
            avatarSeed = avatarSeed,
            // Section 2.1 — `Message` is always offered; from the conversation
            // it simply returns to it, which from a layer means closing it.
            onMessage = onDismiss.takeIf { InfoSection.MESSAGE in sections },
            onStartCall = callAction,
        )
        if (InfoSection.DELETE_CHAT in sections) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                TextButton(
                    onClick = { confirmingDelete = true },
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) { Text(me.lakko.app.chat.DELETE_CHAT_CONTROL) }
                Spacer(Modifier.height(16.dp))
            }
        }
    }
}

/**
 * A person this device holds no contact record for.
 *
 * It shows what is knowable without that record and says so. It does NOT fall
 * back to the group panel, which is the whole defect this replaced: a
 * conversation between two people has no name to set, no link to mint and
 * nothing to leave but the person.
 *
 * No safety number here, and that is not an omission to fix later: the number
 * is computed against a contact this device holds, so offering one it cannot
 * compute would be worse than offering none.
 */
@Composable
private fun UnknownPersonBody(
    name: String,
    username: String?,
    avatarSeed: String,
    onMessage: (() -> Unit)?,
    onStartCall: ((video: Boolean) -> Unit)?,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        LakkoAvatar(name = name, seed = avatarSeed, size = 72.dp)
        Spacer(Modifier.height(12.dp))
        Text(
            name,
            style = MaterialTheme.typography.headlineSmall,
            textAlign = TextAlign.Center,
        )
        username?.let {
            Text(
                "@$it",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Spacer(Modifier.height(20.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            if (onMessage != null) {
                ActionButton(
                    label = "Message",
                    icon = Icons.AutoMirrored.Filled.Chat,
                    enabled = true,
                    onClick = onMessage,
                    modifier = Modifier.weight(1f),
                )
            }
            ActionButton(
                label = "Call",
                icon = Icons.Filled.Call,
                enabled = onStartCall != null,
                onClick = { onStartCall?.invoke(false) },
                modifier = Modifier.weight(1f),
            )
            ActionButton(
                label = "Video",
                icon = Icons.Filled.Videocam,
                enabled = onStartCall != null,
                onClick = { onStartCall?.invoke(true) },
                modifier = Modifier.weight(1f),
            )
        }
        Spacer(Modifier.height(16.dp))
        Text(
            "Not in your contacts. Add them to compare a safety number.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(24.dp))
    }
}
