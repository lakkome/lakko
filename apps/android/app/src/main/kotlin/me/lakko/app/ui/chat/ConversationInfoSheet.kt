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
import me.lakko.app.chat.ConversationInfo
import me.lakko.app.chat.InfoOrigin
import me.lakko.app.chat.InfoSection
import me.lakko.app.chat.personInfoSections
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.contacts.ActionButton
import me.lakko.app.ui.contacts.ContactDetailScreen
import me.lakko.app.ui.contacts.ContactsViewModel

/**
 * The person a one-to-one conversation opens — docs/ux/conversation-info.md.
 *
 * A layer, not a panel (section 3): the conversation stays where it is behind
 * it, and the sheet's handle and its scrim are the one obvious way out. A
 * `ModalBottomSheet` is the idiom the spec names for Android.
 *
 * What it holds is [ContactDetailScreen], the screen that already exists, in
 * its [InfoOrigin.Conversation] presentation. It is deliberately not a second
 * person screen: two screens about a person are two things to keep in step,
 * and the safety number is the one that must never be the stale copy.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ConversationPersonSheet(
    info: ConversationInfo,
    contactsRepository: ContactsRepository,
    contactsViewModel: ContactsViewModel,
    /** The name the chat header shows, for a person with no contact record. */
    peerName: String,
    /** Their verified username, without the "@", or `null`. */
    peerUsername: String?,
    avatarSeed: String,
    /** Start a call on this conversation. `null` while a call already holds it. */
    onStartCall: ((video: Boolean) -> Unit)?,
    onVerifySafetyNumber: (contactKey: String) -> Unit,
    /** ADR-0045 UX 6.2.4 — forget a conversation this device is out of. */
    onDeleteChat: () -> Unit,
    /** Has this device's own membership in the conversation ended? */
    membershipEnded: Boolean,
    onDismiss: () -> Unit,
) {
    // Section 2.1 / section 4 — what this holds, decided in `me.lakko.app.chat`
    // where the host suite runs it.
    val sections = personInfoSections(
        origin = InfoOrigin.Conversation,
        isContact = info is ConversationInfo.Person,
        membershipEnded = membershipEnded,
    )
    val callAction = onStartCall?.takeIf { InfoSection.Call in sections }
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
        when (info) {
            is ConversationInfo.Person -> ContactDetailScreen(
                contactKey = info.contactKey,
                contactsRepository = contactsRepository,
                viewModel = contactsViewModel,
                // The contact screen closes itself when the contact goes; here
                // that means closing the layer, since there is no stack to pop.
                onBack = onDismiss,
                // Section 2.1 — no `Message` from the conversation it would
                // open, so nothing here can be asked to open one.
                onOpenChat = null,
                onStartCall = callAction?.let { start -> { _, video -> start(video) } },
                onVerifySafetyNumber = { key ->
                    // One safety-number screen, reached from the layer by
                    // closing it first: the chat is still behind, so Back from
                    // the number lands on the conversation.
                    onDismiss()
                    onVerifySafetyNumber(key)
                },
                origin = InfoOrigin.Conversation,
                membershipEnded = membershipEnded,
            )

            ConversationInfo.PersonUnknown -> UnknownPersonBody(
                name = peerName,
                username = peerUsername,
                avatarSeed = avatarSeed,
                onStartCall = callAction,
            )

            // The group has its own layer; the header never routes a group here.
            ConversationInfo.Group -> Unit
        }
        if (InfoSection.DeleteChat in sections) {
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
 * A person this device holds no contact record for — a chat that outlived its
 * contact, which removing a contact deliberately leaves behind.
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
