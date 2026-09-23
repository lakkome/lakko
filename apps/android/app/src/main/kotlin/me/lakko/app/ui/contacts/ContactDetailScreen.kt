// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Chat
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.PersonRemove
import androidx.compose.material.icons.filled.VerifiedUser
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import me.lakko.app.chat.InfoOrigin
import me.lakko.app.chat.InfoSection
import me.lakko.app.chat.personInfoSections
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.components.LakkoAvatar

/**
 * Contact detail — docs/ux/client-redesign.md section 5.3, and the person a
 * one-to-one conversation opens (docs/ux/conversation-info.md section 2.1).
 *
 * A 72 dp avatar, the name and the verified username; Message, Call and Video
 * actions (the last two only when a chat exists on this device — a call rides
 * the chat's group, so without one there is nothing to dial and the buttons
 * are disabled rather than faking a state); then rows for the safety number,
 * a rename and removal. No key fingerprint anywhere: the safety number is the
 * verification surface.
 *
 * One screen, two presentations. [InfoOrigin.Elsewhere] is the destination the
 * contacts list opens: its own top bar, its own back arrow, and `Message`,
 * because from there the conversation is somewhere to go. [InfoOrigin.Conversation]
 * is the layer a chat's header opens over the conversation: the host supplies
 * the chrome, and `Message` is gone because the conversation is already behind
 * it. Which rows those are is [personInfoSections], not a flag read here — the
 * defect this replaced was exactly a panel reading flags.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ContactDetailScreen(
    /** The route key: base64url of the contact's signature key. */
    contactKey: String,
    contactsRepository: ContactsRepository,
    viewModel: ContactsViewModel,
    onBack: () -> Unit,
    /** Open the contact's chat. Never called when the info opened from it. */
    onOpenChat: ((groupIdHex: String) -> Unit)?,
    /** Start a 1x1 call on the contact's chat. `null` hides the actions. */
    onStartCall: ((groupIdHex: String, video: Boolean) -> Unit)?,
    onVerifySafetyNumber: (contactKey: String) -> Unit,
    /** Where this was opened from; see the KDoc above. */
    origin: InfoOrigin = InfoOrigin.Elsewhere,
    /** Has this device's membership in the conversation ended? Only a layer
     *  over a conversation can know; a contact opened from the list cannot,
     *  and has no conversation in question. */
    membershipEnded: Boolean = false,
    modifier: Modifier = Modifier,
) {
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    val contact: AcceptedContact? = remember(contactsState.accepted, contactKey) {
        contactsState.accepted.firstOrNull { contactRouteKey(it.request.fromPubkey) == contactKey }
    }
    // Removed (here or on another screen) while open: nothing to show.
    LaunchedEffect(contact) { if (contact == null) onBack() }

    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    // Opening the detail retries a stalled username verification.
    LaunchedEffect(contactKey) {
        contact?.let { directory.ensureVerified(java.util.Base64.getEncoder().encodeToString(it.request.fromPubkey)) }
    }

    var renaming by remember { mutableStateOf(false) }
    var draftName by remember { mutableStateOf("") }
    var confirmingRemove by remember { mutableStateOf(false) }

    // Section 2.1 — which rows this holds, decided once in `me.lakko.app.chat`
    // where the host suite can run it, never by a flag read down in the layout.
    val sections = personInfoSections(origin, isContact = true, membershipEnded = membershipEnded)
    // Read through `nameVersion` so a profile or a username arriving later
    // renames in place.
    val resolvedName = contact?.let { c -> nameVersion.let { directory.resolve(c.request.fromPubkey) } }

    val body: @Composable (Modifier) -> Unit = { bodyModifier ->
        val c = contact
        if (c != null && resolvedName != null) {
            ContactDetailBody(
                contactKey = contactKey,
                name = resolvedName,
                groupIdHex = c.groupIdHex,
                sections = sections,
                onOpenChat = onOpenChat,
                onStartCall = onStartCall,
                onVerifySafetyNumber = onVerifySafetyNumber,
                onRename = {
                    draftName = c.displayName ?: ""
                    renaming = true
                },
                onRemove = { confirmingRemove = true },
                modifier = bodyModifier,
            )
        }
    }

    when (origin) {
        // The layer over a conversation: the sheet that hosts this draws the
        // handle and the way out, so a second top bar inside it would be a
        // second way out of one thing.
        InfoOrigin.Conversation -> body(
            Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState()),
        )

        InfoOrigin.Elsewhere -> Scaffold(
            modifier = modifier,
            topBar = {
                TopAppBar(
                    title = {},
                    navigationIcon = {
                        IconButton(onClick = onBack) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                        }
                    },
                )
            },
        ) { padding ->
            body(
                Modifier
                    .fillMaxSize()
                    .padding(padding)
                    .verticalScroll(rememberScrollState()),
            )
        }
    }

    // Both dialogs belong to the screen rather than to the body, so they are
    // the same dialog in either presentation.
    if (contact != null && resolvedName != null) {
        RenameContactDialog(
            open = renaming,
            draft = draftName,
            onDraftChange = { draftName = it },
            onDismiss = { renaming = false },
            onSave = {
                viewModel.onRenameContactClicked(contact.request.messageId, draftName)
                renaming = false
            },
        )
        RemoveContactDialog(
            open = confirmingRemove,
            name = resolvedName.primary,
            onDismiss = { confirmingRemove = false },
            onRemove = {
                confirmingRemove = false
                viewModel.onRemoveContactClicked(contact.request.messageId)
                onBack()
            },
        )
    }
}

/**
 * The person, as both presentations draw them: identity, then the actions,
 * then the detail — the common shape docs/ux/conversation-info.md section 2
 * asks the three info surfaces to share.
 */
@Composable
private fun ContactDetailBody(
    contactKey: String,
    name: me.lakko.app.profile.ResolvedName,
    groupIdHex: String?,
    sections: Set<InfoSection>,
    onOpenChat: ((String) -> Unit)?,
    onStartCall: ((groupIdHex: String, video: Boolean) -> Unit)?,
    onVerifySafetyNumber: (contactKey: String) -> Unit,
    onRename: () -> Unit,
    onRemove: () -> Unit,
    modifier: Modifier = Modifier,
) {
        Column(
            modifier = modifier,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Spacer(Modifier.height(8.dp))
            LakkoAvatar(
                name = if (name.isPlaceholder) "?" else name.primary,
                seed = contactKey,
                size = 72.dp,
            )
            Spacer(Modifier.height(12.dp))
            Text(name.primary, style = MaterialTheme.typography.headlineSmall, textAlign = TextAlign.Center)
            name.verifiedUsername?.takeIf { name.primary != "@$it" }?.let { user ->
                Text(
                    "@$user",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            name.theirName?.let { theirs ->
                Text(
                    "Their name: $theirs",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (name.usernameNotVerified) {
                Text(
                    "Username not verified",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(20.dp))

            Row(
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                modifier = Modifier.padding(horizontal = 16.dp),
            ) {
                // Opened from the conversation itself, a button that opens the
                // conversation is noise; opened from the contacts list it is
                // the point. Section 2.1.
                if (InfoSection.Message in sections) {
                    ActionButton(
                        label = "Message",
                        icon = Icons.AutoMirrored.Filled.Chat,
                        enabled = groupIdHex != null && onOpenChat != null,
                        onClick = { if (groupIdHex != null) onOpenChat?.invoke(groupIdHex) },
                        modifier = Modifier.weight(1f),
                    )
                }
                // Section 4 — a conversation this device's membership has
                // ended in has no call to place, so the tiles are gone rather
                // than dimmed: the key comes from a group we are not in.
                if (InfoSection.Call in sections) {
                    ActionButton(
                        label = "Call",
                        icon = Icons.Filled.Call,
                        enabled = groupIdHex != null && onStartCall != null,
                        onClick = { if (groupIdHex != null) onStartCall?.invoke(groupIdHex, false) },
                        modifier = Modifier.weight(1f),
                    )
                    ActionButton(
                        label = "Video",
                        icon = Icons.Filled.Videocam,
                        enabled = groupIdHex != null && onStartCall != null,
                        onClick = { if (groupIdHex != null) onStartCall?.invoke(groupIdHex, true) },
                        modifier = Modifier.weight(1f),
                    )
                }
            }
            // The line exists to explain a dimmed tile, so it goes when there
            // is no tile to explain.
            val hasConversationActions =
                InfoSection.Call in sections || InfoSection.Message in sections
            if (groupIdHex == null && hasConversationActions) {
                Spacer(Modifier.height(8.dp))
                Text(
                    "Chat not available on this device yet.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(24.dp))
            HorizontalDivider()

            if (InfoSection.SafetyNumber in sections) {
                DetailRow(
                    title = "Verify safety number",
                    supporting = "Compare a number in person to confirm this is really them.",
                    icon = Icons.Filled.VerifiedUser,
                    onClick = { onVerifySafetyNumber(contactKey) },
                )
            }
            if (InfoSection.RenameContact in sections) {
                DetailRow(
                    title = "Rename",
                    supporting = "Your name for this contact. Only you see it.",
                    icon = Icons.Filled.Edit,
                    onClick = onRename,
                )
            }
            if (InfoSection.RemoveContact in sections) {
                HorizontalDivider()
                DetailRow(
                    title = "Remove contact",
                    supporting = null,
                    icon = Icons.Filled.PersonRemove,
                    onClick = onRemove,
                    danger = true,
                )
            }
            Spacer(Modifier.height(24.dp))
        }
}

/**
 * The local rename — the viewer's own name for a contact, which nobody else
 * ever sees. Extracted so both presentations of the screen raise one dialog.
 */
@Composable
private fun RenameContactDialog(
    open: Boolean,
    draft: String,
    onDraftChange: (String) -> Unit,
    onDismiss: () -> Unit,
    onSave: () -> Unit,
) {
    if (!open) return
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Rename") },
        text = {
            Column {
                Text(
                    "Leave blank to use their own name.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = draft,
                    onValueChange = onDraftChange,
                    singleLine = true,
                    label = { Text("Name") },
                )
            }
        },
        confirmButton = { TextButton(onClick = onSave) { Text("Save") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** Removing a contact, which deliberately leaves any existing chat behind. */
@Composable
private fun RemoveContactDialog(
    open: Boolean,
    name: String,
    onDismiss: () -> Unit,
    onRemove: () -> Unit,
) {
    if (!open) return
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Remove $name?") },
        text = {
            Text(
                "They are removed from your contacts on this device. " +
                    "An existing chat stays until you leave it.",
            )
        },
        confirmButton = {
            TextButton(onClick = onRemove) {
                Text("Remove", color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** One action tile. `internal` so the conversation layer draws the same one. */
@Composable
internal fun ActionButton(
    label: String,
    icon: ImageVector,
    enabled: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // Three tiles share one row on a 360 dp phone; the default button padding
    // left "Message" wrapping onto two lines, so the tile keeps a single line
    // and trims its horizontal padding instead.
    FilledTonalButton(
        onClick = onClick,
        enabled = enabled,
        modifier = modifier,
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 8.dp, vertical = 10.dp),
    ) {
        Icon(icon, contentDescription = null)
        Spacer(Modifier.width(6.dp))
        Text(label, maxLines = 1, softWrap = false)
    }
}

@Composable
private fun DetailRow(
    title: String,
    supporting: String?,
    icon: ImageVector,
    onClick: () -> Unit,
    danger: Boolean = false,
) {
    val tint = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant
    ListItem(
        headlineContent = {
            Text(title, color = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
        },
        supportingContent = supporting?.let { { Text(it) } },
        leadingContent = { Icon(icon, contentDescription = null, tint = tint) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier.clickable(onClick = onClick),
    )
}

// =============================================================================
// Safety number
// =============================================================================

private sealed interface SafetyNumberUi {
    data object Loading : SafetyNumberUi
    data class Ready(val groups: List<String>) : SafetyNumberUi
    data class Error(val message: String) : SafetyNumberUi
}

/**
 * The safety number between this account and a contact — ADR-0005 section 13.5
 * — in five-digit groups, large, with a one-paragraph explanation. Both people
 * see the same number; comparing it out of band confirms nobody sits between
 * them. There is no "verified" toggle: this client keeps no per-contact
 * verification mark yet, so none is shown.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun SafetyNumberScreen(
    contactKey: String,
    contactsRepository: ContactsRepository,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    val contact = remember(contactsState.accepted, contactKey) {
        contactsState.accepted.firstOrNull { contactRouteKey(it.request.fromPubkey) == contactKey }
    }
    val appContext = LocalContext.current.applicationContext
    val directory = remember(appContext) { ProfileDirectory.get(appContext) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val name = contact?.let { nameVersion.let { _ -> directory.resolve(it.request.fromPubkey).primary } }

    var ui: SafetyNumberUi by remember { mutableStateOf(SafetyNumberUi.Loading) }
    LaunchedEffect(contactKey, contact) {
        val c = contact ?: return@LaunchedEffect
        ui = try {
            // Derives the own identity from the seed; keep it off the main thread.
            val raw = withContext(Dispatchers.Default) { contactsRepository.safetyNumber(c.request.fromPubkey) }
            SafetyNumberUi.Ready(safetyNumberGroups(raw))
        } catch (cause: Throwable) {
            SafetyNumberUi.Error("Could not compute the safety number. Try again later.")
        }
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text("Safety number") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 24.dp, vertical = 16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            when (val s = ui) {
                SafetyNumberUi.Loading -> CircularProgressIndicator(modifier = Modifier.padding(32.dp))
                is SafetyNumberUi.Error -> Text(
                    s.message,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
                is SafetyNumberUi.Ready -> {
                    Spacer(Modifier.height(8.dp))
                    for (row in safetyNumberRows(s.groups)) {
                        Row(horizontalArrangement = Arrangement.spacedBy(20.dp)) {
                            for (group in row) {
                                Text(
                                    text = group,
                                    fontFamily = FontFamily.Monospace,
                                    fontWeight = FontWeight.SemiBold,
                                    fontSize = 24.sp,
                                    letterSpacing = 1.sp,
                                )
                            }
                        }
                        Spacer(Modifier.height(10.dp))
                    }
                }
            }
            Spacer(Modifier.height(16.dp))
            Text(
                text = "Compare this number with ${name ?: "your contact"} in person or on a call " +
                    "you trust. Both of you see the same number. If it matches, nobody is " +
                    "between you; if it does not, stop and check before sharing anything.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
    }
}
