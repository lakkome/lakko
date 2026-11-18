// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.checklistViewOf
import me.lakko.app.chat.outboxB64
import me.lakko.app.l10n.t
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import uniffi.lakko_ffi_uniffi.AttachmentMenuItem
import uniffi.lakko_ffi_uniffi.MessageContent

/** Which Lakko content screen is open over the conversation. */
internal sealed interface ContentScreen {
    data object Poll : ContentScreen
    data object Checklist : ContentScreen
    data class EditChecklist(val target: Long, val draft: ChecklistDraft) : ContentScreen
    data object Location : ContentScreen
    data object Contact : ContentScreen
    data object Article : ContentScreen
    data class EditArticle(val target: Long, val title: String, val blocks: List<EditBlock>) : ContentScreen
    data class ReadArticle(val messageId: Long) : ContentScreen
    data class PollResults(val messageId: Long) : ContentScreen
    data class Notice(val text: String) : ContentScreen
    data class ConfirmClose(val messageId: Long, val poll: Boolean) : ContentScreen
}

/** ADR-0092 section 3.1 — the poll body from the editor's draft. */
internal fun pollBodyOf(draft: PollDraft, ringHash: ByteArray?, nowMs: Long): uniffi.lakko_ffi_uniffi.PollBody? {
    val random = java.security.SecureRandom()
    val id = ByteArray(16).also(random::nextBytes)
    val anonymous = if (draft.anonymous) {
        val hash = ringHash ?: return null
        uniffi.lakko_ffi_uniffi.PollAnonymous(ByteArray(32).also(random::nextBytes), hash)
    } else {
        null
    }
    return uniffi.lakko_ffi_uniffi.PollBody(
        id = id,
        question = draft.question,
        options = draft.options,
        multiple = draft.multiple,
        quiz = draft.quizCorrect?.let { uniffi.lakko_ffi_uniffi.PollQuiz(it.toUByte(), draft.explanation) },
        closeAtMs = draft.closeAfterMs?.let { nowMs + it },
        anonymous = anonymous,
    )
}

/** ADR-0092 section 5 — the checklist body from the editor's draft (item ids 0..n). */
internal fun checklistBodyOf(draft: ChecklistDraft): uniffi.lakko_ffi_uniffi.ChecklistBody =
    uniffi.lakko_ffi_uniffi.ChecklistBody(
        title = draft.title,
        othersAdd = draft.othersAdd,
        othersMark = draft.othersMark,
        items = draft.items.mapIndexed { i, text -> uniffi.lakko_ffi_uniffi.ChecklistItem(i.toUInt(), text) },
    )

/** Whether the device has a camera (UX 2.4). */
private fun hasCamera(context: android.content.Context): Boolean =
    context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY)

/** Whether the device has a location service (UX 2.4). */
private fun hasLocation(context: android.content.Context): Boolean =
    context.packageManager.hasSystemFeature(PackageManager.FEATURE_LOCATION)

/** UX 9.3 — Android opens a `geo:` link: the default maps app, or the chooser. */
internal fun openGeo(context: android.content.Context, loc: uniffi.lakko_ffi_uniffi.SharedLocation) {
    try {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(geoUriOf(loc))).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    } catch (_: ActivityNotFoundException) {
        // No maps app: nothing to open; Copy coordinates stays in the message menu.
    }
}

@Composable
internal fun rememberContentBubbleContext(
    viewModel: ChatViewModel,
    contactsRepository: me.lakko.app.contacts.ContactsRepository,
    ownKey: me.lakko.app.chat.MemberIdentity?,
    isOneToOne: Boolean,
    onScreen: (ContentScreen) -> Unit,
    onOpenContact: (String) -> Unit,
    mediaHub: me.lakko.app.media.ChatMediaHub? = null,
): ContentBubbleContext {
    val context = LocalContext.current
    val contacts by contactsRepository.state.collectAsState()
    val scope = rememberCoroutineScope()
    val groupId = remember(viewModel.groupIdHex) { viewModel.contentGroupId() }
    val admin = viewModel.isOwnerOrAdmin()
    return ContentBubbleContext(
        groupId = groupId,
        ownKey = ownKey,
        isOwnerOrAdmin = admin,
        isOneToOne = isOneToOne,
        nameOf = { key -> viewModel.nameOf(key) },
        onVote = viewModel::onPollVote,
        onRetryVote = viewModel::onRetryVote,
        onViewResults = { msg -> onScreen(ContentScreen.PollResults(msg.messageId)) },
        onTick = viewModel::onChecklistTick,
        onAddItem = viewModel::onChecklistAdd,
        onOpenLocation = { openGeo(context, it) },
        contactCardAction = { key ->
            when {
                ownKey != null && outboxB64(key) == ownKey -> ContactCardAction.THIS_IS_YOU
                contacts.accepted.any { it.request.fromPubkey.contentEquals(key) } -> ContactCardAction.OPEN_CHAT
                else -> ContactCardAction.ADD
            }
        },
        onContactCard = { card, action ->
            when (action) {
                ContactCardAction.OPEN_CHAT -> onOpenContact(me.lakko.app.ui.contacts.contactRouteKey(card.accountKey))
                ContactCardAction.THIS_IS_YOU -> Unit
                ContactCardAction.ADD -> scope.launch {
                    // UX 10.2 — the ordinary username lookup, then the
                    // ordinary request when the name still names this key.
                    val outcome = try {
                        val (_, key) = contactsRepository.resolveVerifiedRecipient(card.username)
                        if (key.contentEquals(card.accountKey)) {
                            contactsRepository.sendContactRequest(card.username, "")
                            null
                        } else {
                            me.lakko.app.l10n.Localization.strings.value["contact.username_changed"]
                        }
                    } catch (e: Throwable) {
                        me.lakko.app.rethrowIfCancelled(e)
                        me.lakko.app.l10n.Localization.strings.value.format(
                            "contact.not_findable",
                            mapOf("name" to card.name, "sender" to card.name),
                        )
                    }
                    if (outcome != null) onScreen(ContentScreen.Notice(outcome))
                }
            }
        },
        onOpenArticle = { msg -> onScreen(ContentScreen.ReadArticle(msg.messageId)) },
        syncedChoiceOf = { id -> viewModel.ownPollChoice(id)?.choices },
        banner = mediaHub?.let { hub -> articleBanner(hub) },
        onClose = { msg -> onScreen(ContentScreen.ConfirmClose(msg.messageId, msg.content is MessageContent.Poll)) },
        onEditChecklist = { msg ->
            checklistViewOf(msg)?.let { v ->
                onScreen(
                    ContentScreen.EditChecklist(
                        msg.messageId,
                        ChecklistDraft(v.title, v.items.map { it.text }, v.othersAdd, v.othersMark),
                    ),
                )
            }
        },
        onEditArticle = { msg ->
            me.lakko.app.chat.articleOf(msg)?.let { (a, images) ->
                onScreen(ContentScreen.EditArticle(msg.messageId, a.title, editBlocksOf(a, images)))
            }
        },
        onCopyText = { text ->
            val clip = context.getSystemService(android.content.Context.CLIPBOARD_SERVICE) as? android.content.ClipboardManager
            clip?.setPrimaryClip(android.content.ClipData.newPlainText("", text))
        },
    )
}

/**
 * ADR-0092 / attachments UX — the paperclip's menu and every Lakko screen it
 * opens, drawn over the conversation.
 */
@Composable
internal fun AttachmentSurfaces(
    viewModel: ChatViewModel,
    contactsRepository: me.lakko.app.contacts.ContactsRepository,
    menuOpen: Boolean,
    onMenuDismiss: () -> Unit,
    screen: ContentScreen?,
    onScreen: (ContentScreen?) -> Unit,
    onPickVisual: () -> Unit,
    onPickFiles: () -> Unit,
    onPickAudio: () -> Unit,
    onOpenContact: (String) -> Unit,
    callHoldsCamera: Boolean,
    mediaHub: me.lakko.app.media.ChatMediaHub,
) {
    val context = LocalContext.current
    var cameraOpen by remember { mutableStateOf(false) }
    if (menuOpen) {
        val rows = remember(menuOpen) {
            viewModel.attachmentMenuRows(hasCamera(context), hasLocation(context), callHoldsCamera)
        }
        AttachmentMenuSheet(
            rows = rows,
            onPick = { item ->
                when (item) {
                    AttachmentMenuItem.PHOTO_OR_VIDEO -> onPickVisual()
                    AttachmentMenuItem.CAMERA -> cameraOpen = true
                    AttachmentMenuItem.FILE -> onPickFiles()
                    AttachmentMenuItem.AUDIO -> onPickAudio()
                    AttachmentMenuItem.POLL -> onScreen(ContentScreen.Poll)
                    AttachmentMenuItem.CHECKLIST -> onScreen(ContentScreen.Checklist)
                    AttachmentMenuItem.LOCATION -> onScreen(ContentScreen.Location)
                    AttachmentMenuItem.CONTACT -> onScreen(ContentScreen.Contact)
                    AttachmentMenuItem.ARTICLE -> onScreen(ContentScreen.Article)
                }
            },
            onDismiss = onMenuDismiss,
        )
    }
    if (cameraOpen) {
        CameraCaptureScreen(
            onCaptured = { uri -> viewModel.openSendPreview(listOf(uri), filesOnly = false) },
            onClose = { cameraOpen = false },
        )
    }
    val preview by viewModel.sendPreview.collectAsState()
    preview?.let { p ->
        SendPreviewScreen(
            preview = p,
            onCaption = viewModel::onPreviewCaption,
            onSendAsFile = viewModel::onPreviewSendAsFile,
            onRemove = viewModel::onPreviewRemove,
            onSend = viewModel::sendPreviewNow,
            onClose = viewModel::closeSendPreview,
        )
    }
    val close = { onScreen(null) }
    when (val s = screen) {
        null -> Unit
        ContentScreen.Poll -> PollEditor(
            anonymity = remember { viewModel.pollAnonymity() },
            onCreate = { draft ->
                val body = pollBodyOf(draft, if (draft.anonymous) viewModel.currentRingHash() else null, System.currentTimeMillis())
                val signed = body?.let(viewModel::signPoll)
                if (signed != null) viewModel.sendContent(MessageContent.Poll(signed))
                close()
            },
            onClose = close,
        )
        ContentScreen.Checklist -> ChecklistEditor(
            initial = null,
            onSave = { draft ->
                viewModel.signChecklist(checklistBodyOf(draft))?.let { viewModel.sendContent(MessageContent.Checklist(it)) }
                close()
            },
            onClose = close,
        )
        is ContentScreen.EditChecklist -> ChecklistEditor(
            initial = s.draft,
            onSave = { draft ->
                viewModel.onEditChecklist(s.target, checklistBodyOf(draft))
                close()
            },
            onClose = close,
        )
        ContentScreen.Location -> LocationScreen(
            onSend = { loc ->
                viewModel.sendContent(MessageContent.Location(loc))
                close()
            },
            onClose = close,
        )
        ContentScreen.Contact -> {
            val contacts by contactsRepository.state.collectAsState()
            ContactPickerScreen(
                contacts = me.lakko.app.ui.chat.contactPickRows(contacts.accepted) { key ->
                    me.lakko.app.profile.ProfileDirectory.get(context).profiles.get(key)
                },
                onSend = { row ->
                    if (!row.pickable) return@ContactPickerScreen
                    val username = row.username ?: return@ContactPickerScreen
                    viewModel.sendContent(
                        MessageContent.ContactCard(
                            uniffi.lakko_ffi_uniffi.SharedContactCard(row.name, username, row.accountKey),
                        ),
                    )
                    close()
                },
                onClose = close,
            )
        }
        ContentScreen.Article -> ArticleEditor(
            conversation = viewModel.groupIdHex,
            initial = null,
            editing = false,
            uploadImage = viewModel::uploadArticleImage,
            onPublish = { article, images ->
                viewModel.sendContent(MessageContent.Article(article), images)
                close()
            },
            onClose = close,
        )
        is ContentScreen.EditArticle -> ArticleEditor(
            conversation = viewModel.groupIdHex,
            initial = s.title to s.blocks,
            editing = true,
            uploadImage = viewModel::uploadArticleImage,
            onPublish = { article, images ->
                viewModel.onEditArticle(s.target, article, images)
                close()
            },
            onClose = close,
        )
        is ContentScreen.ReadArticle -> {
            val msg = viewModel.contentRow(s.messageId)
            val pair = msg?.let { me.lakko.app.chat.articleOf(it) }
            if (msg == null || pair == null) {
                close()
            } else {
                val hub = mediaHub
                ArticleReader(
                    article = pair.first,
                    images = pair.second,
                    author = msg.authorKey?.let(viewModel::nameOf) ?: "",
                    date = msg.createdAt.take(10),
                    image = { ref ->
                        AttachmentView(
                            ref = ref,
                            hub = hub,
                            isOwn = false,
                            contentColor = MaterialTheme.colorScheme.onBackground,
                            bubbleColor = MaterialTheme.colorScheme.background,
                            fileChip = {},
                        )
                    },
                    onOpenLink = { url ->
                        try {
                            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                        } catch (_: ActivityNotFoundException) {
                        }
                    },
                    onMenu = if (msg.authorKey != null && msg.authorKey == viewModel.ownMemberKey()) {
                        { onScreen(ContentScreen.EditArticle(msg.messageId, pair.first.title, editBlocksOf(pair.first, pair.second))) }
                    } else {
                        null
                    },
                    onClose = close,
                )
            }
        }
        is ContentScreen.PollResults -> {
            val msg = viewModel.contentRow(s.messageId)
            if (msg == null) close() else PollResultsScreen(msg, viewModel, close)
        }
        is ContentScreen.ConfirmClose -> LakkoDialog(
            onDismissRequest = close,
            title = t(if (s.poll) "poll.close_confirm" else "checklist.close_confirm"),
            confirm = LakkoDialogAction(t(if (s.poll) "poll.close" else "checklist.close"), LakkoDialogTone.Danger) {
                viewModel.onCloseContent(s.messageId)
                close()
            },
        )
        is ContentScreen.Notice -> LakkoDialog(
            onDismissRequest = close,
            title = s.text,
            confirm = LakkoDialogAction(t("common.ok"), LakkoDialogTone.Primary) { close() },
            cancel = null,
        )
    }
}

/** UX 7.4 — who chose what, for a poll that is not anonymous. */
@Composable
private fun PollResultsScreen(msg: ChatMessage, viewModel: ChatViewModel, onClose: () -> Unit) {
    val poll = (msg.content as? MessageContent.Poll)?.poll?.body ?: return
    val author = me.lakko.app.chat.identityBytes(msg.authorKey) ?: ByteArray(32)
    val tally = remember(msg) {
        try {
            uniffi.lakko_ffi_uniffi.foldPoll(
                poll,
                author,
                msg.messageId,
                msg.contentExtras.ops.filter { it.kind == me.lakko.app.chat.ContentOpKind.PollVote }.mapNotNull { op ->
                    me.lakko.app.chat.identityBytes(op.sender)?.let {
                        uniffi.lakko_ffi_uniffi.PollVoteRecord(op.opId, op.createdAtMs, it, op.choices ?: ByteArray(0))
                    }
                },
                emptyList(),
            )
        } catch (_: Throwable) {
            null
        }
    }
    ContentEditorScaffold(
        title = t("poll.results.title"),
        actionLabel = t("common.close"),
        actionEnabled = true,
        onAction = onClose,
        onClose = onClose,
    ) {
        Text(poll.question, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
        for ((i, option) in poll.options.withIndex()) {
            val voters = tally?.choicesByVoter?.filter { v -> v.choices.any { (it.toInt() and 0xff) == i } }.orEmpty()
            Column(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Row {
                    Text(option, fontSize = 15.sp, fontWeight = FontWeight.Medium, modifier = Modifier.weight(1f))
                    Text(voters.size.toString(), fontSize = 15.sp, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                for (v in voters) {
                    val name = viewModel.nameOf(outboxB64(v.voter))
                    Row(Modifier.heightIn(min = 40.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        me.lakko.app.ui.components.LakkoAvatar(name = name, size = 32.dp, personKey = v.voter)
                        Text(name, fontSize = 15.sp)
                    }
                }
            }
        }
    }
}

/** UX 11.2 — the banner image of an article card, drawn as a photo attachment. */
internal fun articleBanner(hub: me.lakko.app.media.ChatMediaHub): @Composable (uniffi.lakko_ffi_uniffi.AttachmentRef) -> Unit = { ref ->
    AttachmentView(
        ref = ref,
        hub = hub,
        isOwn = false,
        contentColor = MaterialTheme.colorScheme.onSurface,
        bubbleColor = MaterialTheme.colorScheme.surfaceVariant,
        fileChip = {},
    )
}
