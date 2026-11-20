// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import android.util.Log
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.calls.ContactInviteOutcome
import me.lakko.app.calls.inviteContactRows
import me.lakko.app.calls.inviteContactToCall
import me.lakko.app.chat.ChatRepository
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.l10n.UiText
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.call.InviteContactsPicker
import me.lakko.app.ui.call.callInviteContactPorts
import me.lakko.app.ui.contacts.ContactRowModel
import me.lakko.app.ui.contacts.contactRouteKey

/**
 * ADR-0087 section 14.1 (and 15.2 for a publisher) — `Invite people`: the
 * contact picker, every contact. Each pick sends one message into the
 * one-to-one with that person, started if there is none through the order
 * `Message` uses ([inviteContactToCall] over the call picker's ports): a
 * working grant founds it, else a contact request goes inside a shared
 * group, else nothing. [linkFor] mints the link a pick sends (a private
 * channel: one link per person); `null` from it is a failed send.
 */
@Composable
internal fun ChannelInviteContactsDialog(
    chatRepository: ChatRepository,
    contactsRepository: ContactsRepository,
    linkFor: suspend (name: String) -> me.lakko.app.channels.ChannelLinkMint?,
    onClose: () -> Unit,
    /** True for `Add publisher` (section 15.2): the title says so instead
     *  of `Invite people`. */
    publisher: Boolean = false,
    /** Account keys (hex) known to be in the channel already: the owner, the
     *  publishers and, for `Invite people`, subscribers known by their card.
     *  Not offered (sections 14.1, 15.2). */
    inChannelHex: Set<String> = emptySet(),
) {
    val scope = rememberCoroutineScope()
    var busyKey by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<UiText?>(null) }
    var invited by remember { mutableStateOf(emptySet<String>()) }
    var requested by remember { mutableStateOf(emptySet<String>()) }
    var cannotMessage by remember { mutableStateOf(emptySet<String>()) }
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    val directory = remember(chatRepository) { ProfileDirectory.get(chatRepository.context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val rows = remember(contactsState.accepted, nameVersion, inChannelHex) {
        inviteContactRows(
            withoutInChannel(contactsState.accepted, { it.request.fromPubkey }, inChannelHex).map { contact ->
                val resolved = directory.resolve(contact.request.fromPubkey)
                ContactRowModel(
                    key = contactRouteKey(contact.request.fromPubkey),
                    name = resolved.primary,
                    username = resolved.verifiedUsername,
                    placeholder = resolved.isPlaceholder,
                )
            },
        )
    }
    // Section 14 — the invitation goes as the link text with its card beside
    // it; the card is the one the pick just minted.
    val pendingCard = remember { java.util.concurrent.atomic.AtomicReference<ByteArray?>(null) }
    val ports = remember(chatRepository, contactsRepository) {
        val base = callInviteContactPorts(chatRepository, contactsRepository)
        object : me.lakko.app.calls.InviteContactPorts by base {
            override suspend fun sendText(groupIdHex: String, text: String) =
                chatRepository.sendMessage(groupIdHex = groupIdHex, plaintext = text, channelInvite = pendingCard.get())
        }
    }
    Dialog(
        onDismissRequest = { if (busyKey == null) onClose() },
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        InviteContactsPicker(
            rows = rows,
            busyKey = busyKey,
            invitedKeys = invited,
            requestSentKeys = requested,
            cannotMessageKeys = cannotMessage,
            error = error,
            onBack = onClose,
            titleKey = channelInvitePickerTitleKey(publisher),
            onInvite = { row ->
                if (busyKey == null && row.key !in invited && row.key !in requested) {
                    busyKey = row.key
                    error = null
                    scope.launch {
                        val outcome = try {
                            inviteContactToCall(ports, row.key) { send ->
                                val minted = linkFor(row.name) ?: return@inviteContactToCall false
                                pendingCard.set(minted.card)
                                try {
                                    send(minted.url)
                                    true
                                } catch (e: kotlinx.coroutines.CancellationException) {
                                    throw e
                                } catch (e: Throwable) {
                                    Log.w("ChannelInvite", "invitation not sent", e)
                                    false
                                }
                            }
                        } finally {
                            busyKey = null
                        }
                        cannotMessage = cannotMessage - row.key
                        when (outcome) {
                            ContactInviteOutcome.SENT -> invited = invited + row.key
                            ContactInviteOutcome.REQUEST_SENT -> requested = requested + row.key
                            ContactInviteOutcome.CANNOT_MESSAGE -> cannotMessage = cannotMessage + row.key
                            ContactInviteOutcome.FAILED ->
                                error = UiText("channels.invite.failed", texts = mapOf("names" to row.name))
                        }
                    }
                }
            },
            modifier = Modifier.fillMaxSize(),
        )
    }
}

/** Sections 14.1 and 15.2 — [items] less every one whose account key is in
 *  [inChannelHex] (lower-case hex), in the order given. Pure. */
internal fun <T> withoutInChannel(items: List<T>, accountKeyOf: (T) -> ByteArray, inChannelHex: Set<String>): List<T> =
    if (inChannelHex.isEmpty()) {
        items
    } else {
        items.filter { me.lakko.app.reach.reachHex(accountKeyOf(it)).lowercase() !in inChannelHex }
    }

/** The picker's title: what the pick does — `Add publisher` or `Invite
 *  people`, the same keys web and iOS use for this screen. */
internal fun channelInvitePickerTitleKey(publisher: Boolean): String =
    if (publisher) "channels.publishers.add" else "channels.info.invite_people"
