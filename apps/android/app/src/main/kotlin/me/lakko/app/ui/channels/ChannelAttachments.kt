// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import android.content.pm.PackageManager
import android.net.Uri
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.ui.chat.ArticleEditor
import me.lakko.app.ui.chat.AttachmentMenuSheet
import me.lakko.app.ui.chat.CameraCaptureScreen
import me.lakko.app.ui.chat.ChecklistDraft
import me.lakko.app.ui.chat.ChecklistEditor
import me.lakko.app.ui.chat.ContactPickRow
import me.lakko.app.ui.chat.ContactPickerScreen
import me.lakko.app.ui.chat.LocationScreen
import me.lakko.app.ui.chat.PollEditor
import me.lakko.app.ui.chat.checklistBodyOf
import me.lakko.app.ui.chat.pollBodyOf
import uniffi.lakko_ffi_uniffi.AttachmentMenuItem
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.MessageContent

/** Which Lakko content screen is open over a channel. */
internal enum class ChannelContentScreen { POLL, CHECKLIST, LOCATION, CONTACT, ARTICLE }

/**
 * ADR-0090 / attachments UX 2.4 and 7.7 — a publisher's attachment menu and
 * its editors in a channel. A poll here is always anonymous (the switch is
 * not shown) and carries no anonymous fields or author signature: the post
 * signature authenticates the publisher (ADR-0090 section 4.1); a checklist
 * is likewise unsigned. A subscriber has no paperclip.
 */
@Composable
internal fun ChannelAttachSurfaces(
    menuOpen: Boolean,
    onMenuDismiss: () -> Unit,
    isOwner: Boolean,
    screen: ChannelContentScreen?,
    onScreen: (ChannelContentScreen?) -> Unit,
    contactsRepository: me.lakko.app.contacts.ContactsRepository?,
    onPickVisual: () -> Unit,
    onPickFiles: () -> Unit,
    onPickAudio: () -> Unit,
    onCaptured: (Uri) -> Unit,
    onPublish: (MessageContent, List<AttachmentRef>) -> Unit,
    /** The channel, which names its article draft. */
    draftKey: String = "channel",
) {
    val context = LocalContext.current
    var cameraOpen by remember { mutableStateOf(false) }
    if (menuOpen) {
        val rows = remember(menuOpen) {
            try {
                uniffi.lakko_ffi_uniffi.attachmentMenu(
                    uniffi.lakko_ffi_uniffi.ConversationKind.CHANNEL,
                    // Owner and publisher get the same rows (the rule only
                    // refuses a subscriber, who has no paperclip at all).
                    if (isOwner) uniffi.lakko_ffi_uniffi.ConversationRole.OWNER else uniffi.lakko_ffi_uniffi.ConversationRole.ADMIN,
                    context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY),
                    context.packageManager.hasSystemFeature(PackageManager.FEATURE_LOCATION),
                    // The app-wide call slot: a call anywhere holds the camera.
                    me.lakko.app.calls.AppCallHost.occupant.value != null,
                )
            } catch (_: Throwable) {
                emptyList()
            }
        }
        AttachmentMenuSheet(
            rows = rows,
            onPick = { item ->
                when (item) {
                    AttachmentMenuItem.PHOTO_OR_VIDEO -> onPickVisual()
                    AttachmentMenuItem.CAMERA -> cameraOpen = true
                    AttachmentMenuItem.FILE -> onPickFiles()
                    AttachmentMenuItem.AUDIO -> onPickAudio()
                    AttachmentMenuItem.POLL -> onScreen(ChannelContentScreen.POLL)
                    AttachmentMenuItem.CHECKLIST -> onScreen(ChannelContentScreen.CHECKLIST)
                    AttachmentMenuItem.LOCATION -> onScreen(ChannelContentScreen.LOCATION)
                    AttachmentMenuItem.CONTACT -> onScreen(ChannelContentScreen.CONTACT)
                    AttachmentMenuItem.ARTICLE -> onScreen(ChannelContentScreen.ARTICLE)
                }
            },
            onDismiss = onMenuDismiss,
        )
    }
    if (cameraOpen) CameraCaptureScreen(onCaptured = onCaptured, onClose = { cameraOpen = false })
    val close = { onScreen(null) }
    when (screen) {
        null -> Unit
        ChannelContentScreen.POLL -> PollEditor(
            anonymity = uniffi.lakko_ffi_uniffi.PollAnonymity.ALWAYS_ANONYMOUS,
            onCreate = { draft ->
                pollBodyOf(draft.copy(anonymous = false), null, System.currentTimeMillis())?.let { body ->
                    onPublish(MessageContent.Poll(uniffi.lakko_ffi_uniffi.SignedPoll(body, null)), emptyList())
                }
                close()
            },
            onClose = close,
        )
        ChannelContentScreen.CHECKLIST -> ChecklistEditor(
            initial = null,
            onSave = { draft: ChecklistDraft ->
                onPublish(
                    MessageContent.Checklist(uniffi.lakko_ffi_uniffi.SignedChecklist(checklistBodyOf(draft.copy(othersAdd = false)), null)),
                    emptyList(),
                )
                close()
            },
            onClose = close,
        )
        ChannelContentScreen.LOCATION -> LocationScreen(
            onSend = { loc ->
                onPublish(MessageContent.Location(loc), emptyList())
                close()
            },
            onClose = close,
        )
        ChannelContentScreen.CONTACT -> {
            val accepted = contactsRepository?.state?.collectAsState()?.value?.accepted.orEmpty()
            ContactPickerScreen(
                contacts = me.lakko.app.ui.chat.contactPickRows(accepted) { key ->
                    me.lakko.app.profile.ProfileDirectory.get(context).profiles.get(key)
                },
                onSend = { row ->
                    row.username?.takeIf { row.pickable }?.let { u ->
                        onPublish(MessageContent.ContactCard(uniffi.lakko_ffi_uniffi.SharedContactCard(row.name, u, row.accountKey)), emptyList())
                    }
                    close()
                },
                onClose = close,
            )
        }
        ChannelContentScreen.ARTICLE -> ArticleEditor(
            conversation = draftKey,
            initial = null,
            editing = false,
            uploadImage = { uri -> uploadChannelArticleImage(context, uri) },
            onPublish = { article, images ->
                onPublish(MessageContent.Article(article), images)
                close()
            },
            onClose = close,
        )
    }
}

/** A channel article image: processed as a photo, uploaded with the channel's file lifetime. */
private suspend fun uploadChannelArticleImage(context: android.content.Context, uri: Uri): AttachmentRef? {
    val read = withContext(Dispatchers.IO) {
        me.lakko.app.media.readPickedUri(context, uri, AttachmentBridge.MAX_ATTACHMENT_BYTES)
    } as? me.lakko.app.media.PickedRead.Ok ?: return null
    val max = runCatching { uniffi.lakko_ffi_uniffi.photoMaxLongestSidePx().toInt() }.getOrDefault(2560)
    val jpeg = withContext(Dispatchers.Default) { me.lakko.app.media.reencodePhoto(read.payload.bytes, max) } ?: return null
    return try {
        AttachmentBridge.encryptAndUpload(bytes = jpeg, mime = "image/jpeg", name = null, idleDays = CHANNEL_FILE_IDLE_DAYS)
    } catch (e: Throwable) {
        me.lakko.app.rethrowIfCancelled(e)
        null
    }
}
