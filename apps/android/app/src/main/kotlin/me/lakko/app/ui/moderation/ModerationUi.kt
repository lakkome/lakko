// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.moderation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.Direction
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.l10n.t
import me.lakko.app.moderation.BlockRegister
import me.lakko.app.moderation.IncludedLine
import me.lakko.app.moderation.ReportAlso
import me.lakko.app.moderation.ReportPhase
import me.lakko.app.moderation.ReportSheetState
import me.lakko.app.moderation.ReportSubject
import me.lakko.app.moderation.ReportableMessage
import me.lakko.app.moderation.REPORT_MESSAGE_LIMIT
import me.lakko.app.moderation.includedLine
import me.lakko.app.moderation.pickable
import me.lakko.app.moderation.reasonKey
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone

/*
 * ADR-0099, UX `reporting-and-moderation.md` sections 1 and 2 — the block
 * dialog, the report sheet and the notice after a report, presented once for
 * the whole app ([ModerationHost] sits beside the app root). Every entry
 * point asks [ModerationRequests]; none draws its own copy of the sheet.
 */

/** A request to block or unblock one person. */
public data class BlockRequest(val accountKeyB64: String, val name: String)

/** A request to report; [leaveOrUnsubscribe] runs the sheet's `Leave the group` / `Unsubscribe`. */
public class ReportRequest(
    public val subject: ReportSubject,
    public val leaveOrUnsubscribe: (() -> Unit)? = null,
    /** A channel-info report: where the channel's posts loaded in memory are. */
    public val channelState: me.lakko.app.channels.ChannelsUiState? = null,
)

public object ModerationRequests {
    private val mutableBlock = MutableStateFlow<BlockRequest?>(null)
    private val mutableReport = MutableStateFlow<ReportRequest?>(null)
    private val mutableNotice = MutableStateFlow<String?>(null)

    public val block: StateFlow<BlockRequest?> = mutableBlock.asStateFlow()
    public val report: StateFlow<ReportRequest?> = mutableReport.asStateFlow()

    /** A catalog key shown briefly at the bottom of the screen. */
    public val notice: StateFlow<String?> = mutableNotice.asStateFlow()

    /** Open the block confirmation (UX section 1.2). */
    public fun askBlock(request: BlockRequest) {
        mutableBlock.value = request
    }

    public fun dismissBlock() {
        mutableBlock.value = null
    }

    /** Open the report sheet (UX section 2.2). */
    public fun openReport(request: ReportRequest) {
        mutableReport.value = request
    }

    public fun dismissReport() {
        mutableReport.value = null
    }

    public fun showNotice(key: String) {
        mutableNotice.value = key
    }

    public fun clearNotice() {
        mutableNotice.value = null
    }
}

/**
 * Block [accountKeyB64] (UX section 1.2): the register first, at once on this
 * device, then the existing Remove contact (which rotates the contact key)
 * and any pending request from them. Unblocking only lifts the register.
 */
public suspend fun applyBlock(
    contactsRepository: ContactsRepository,
    accountKeyB64: String,
    blocked: Boolean,
): Boolean {
    val written = BlockRegister.setBlocked(accountKeyB64, blocked)
    if (!blocked || !written) return written
    val key = try {
        java.util.Base64.getDecoder().decode(me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountKeyB64))
    } catch (_: IllegalArgumentException) {
        return written
    }
    val state = contactsRepository.state.value
    state.accepted.filter { it.request.fromPubkey.contentEquals(key) }
        .forEach { contactsRepository.removeContact(it.request.messageId) }
    state.pending.filter { it.fromPubkey.contentEquals(key) }
        .forEach { contactsRepository.declineContactRequest(it.messageId) }
    return written
}

/** The block, the report sheet and the notice, for the whole app. */
@Composable
public fun ModerationHost(
    contactsRepository: ContactsRepository,
    chatRepository: ChatRepository,
) {
    val scope = rememberCoroutineScope()
    val block by ModerationRequests.block.collectAsStateWithLifecycle()
    val report by ModerationRequests.report.collectAsStateWithLifecycle()
    val notice by ModerationRequests.notice.collectAsStateWithLifecycle()

    block?.let { request ->
        BlockConfirmDialog(
            name = request.name,
            onCancel = ModerationRequests::dismissBlock,
            onBlock = {
                ModerationRequests.dismissBlock()
                scope.launch { applyBlock(contactsRepository, request.accountKeyB64, blocked = true) }
            },
        )
    }
    report?.let { request ->
        ReportSheet(
            request = request,
            chatRepository = chatRepository,
            onAlso = { also ->
                when (also) {
                    is ReportAlso.Block -> scope.launch { applyBlock(contactsRepository, also.accountKeyB64, blocked = true) }
                    is ReportAlso.Leave, is ReportAlso.Unsubscribe -> request.leaveOrUnsubscribe?.invoke()
                    ReportAlso.None -> Unit
                }
            },
            onClose = ModerationRequests::dismissReport,
        )
    }
    notice?.let { key ->
        LaunchedEffect(key) {
            delay(NOTICE_MS)
            ModerationRequests.clearNotice()
        }
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.BottomCenter) {
            Surface(
                shape = RoundedCornerShape(12.dp),
                color = MaterialTheme.colorScheme.inverseSurface,
                modifier = Modifier
                    .navigationBarsPadding()
                    .padding(horizontal = 16.dp, vertical = 24.dp),
            ) {
                Text(
                    t(key),
                    color = MaterialTheme.colorScheme.inverseOnSurface,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
                )
            }
        }
    }
}

private const val NOTICE_MS: Long = 4_000L

/** UX section 1.2 — the confirmation, danger action. */
@Composable
public fun BlockConfirmDialog(name: String, onCancel: () -> Unit, onBlock: () -> Unit) {
    LakkoDialog(
        onDismissRequest = onCancel,
        title = t("block.confirm.title", mapOf("name" to name)),
        body = t("block.confirm.body"),
        confirm = LakkoDialogAction(t("block.action"), LakkoDialogTone.Danger, onClick = onBlock),
        cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost, onClick = onCancel),
    )
}

/** The catalog text of an [IncludedLine]. */
@Composable
private fun includedText(line: IncludedLine): String = when (line.key) {
    "report.included" -> t(line.key, emptyMap(), mapOf("n" to line.n.toULong()))
    "report.included_partial" -> t(
        line.key,
        mapOf("n" to line.n.toString(), "total" to line.total.toString()),
        mapOf("n" to line.n.toULong(), "total" to line.total.toULong()),
    )
    else -> t(line.key)
}

/** UX section 2.2 — the report sheet: why, what's included, anything to add. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ReportSheet(
    request: ReportRequest,
    chatRepository: ChatRepository,
    onAlso: (ReportAlso) -> Unit,
    onClose: () -> Unit,
) {
    val subject = request.subject
    val context = LocalContext.current.applicationContext
    val scope = rememberCoroutineScope()
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    var state by remember(subject) { mutableStateOf(ReportSheetState.initial(subject)) }
    var picking by remember(subject) { mutableStateOf(false) }
    // UX section 2.3 — "Reporting isn't available on this server" is said
    // when the sheet opens and `Send report` stays disabled, so `Also block`
    // never runs against a report that cannot go from this sheet.
    LaunchedEffect(subject) {
        val availability = me.lakko.app.moderation.ModerationClient().use { client ->
            me.lakko.app.moderation.checkReportAvailability(
                me.lakko.app.moderation.ProductionReportPorts(context, subject, state, client),
            )
        }
        if (availability == me.lakko.app.moderation.ReportAvailability.UNAVAILABLE &&
            state.phase != ReportPhase.SENDING
        ) {
            state = state.copy(phase = ReportPhase.UNAVAILABLE)
        }
    }
    val reasons = remember {
        runCatching { uniffi.lakko_ffi_uniffi.reportReasonCodes().map { it.toUByte() } }.getOrDefault(emptyList())
    }

    ModalBottomSheet(
        onDismissRequest = { if (state.phase != ReportPhase.SENDING) onClose() },
        sheetState = sheetState,
    ) {
        if (picking && subject.pickChannelIdHex != null) {
            ReportPostPicker(
                channelIdHex = subject.pickChannelIdHex,
                channelState = request.channelState,
                initial = state.pickedPosts,
                onDone = { posts ->
                    state = state.withPickedPosts(posts)
                    picking = false
                },
            )
            return@ModalBottomSheet
        }
        if (picking && subject.pickGroupIdHex != null) {
            ReportMessagePicker(
                subject = subject,
                chatRepository = chatRepository,
                initial = state.picked,
                onDone = { picked ->
                    state = state.withPicked(picked)
                    picking = false
                },
            )
            return@ModalBottomSheet
        }
        Column(
            Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(bottom = 24.dp),
        ) {
            Text(t("report.title", mapOf("name" to subject.name)), style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.height(16.dp))
            // 1. Why — radio rows, one choice, required.
            Text(t("report.why"), style = MaterialTheme.typography.titleSmall)
            for (code in reasons) {
                val key = reasonKey(uniffi.lakko_ffi_uniffi.reportReasonName(code)) ?: continue
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 48.dp)
                        .selectable(
                            selected = state.reason == code,
                            role = Role.RadioButton,
                            onClick = { state = state.withReason(code) },
                        ),
                ) {
                    RadioButton(selected = state.reason == code, onClick = null)
                    Spacer(Modifier.size(8.dp))
                    Text(t(key), style = MaterialTheme.typography.bodyLarge)
                }
            }
            Spacer(Modifier.height(12.dp))
            HorizontalDivider()
            Spacer(Modifier.height(12.dp))
            // 2. What's included.
            val line = includedLine(state.picked, subject.other.size, state.pickedPosts)
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                Text(includedText(line), style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                if (subject.pickGroupIdHex != null || subject.pickChannelIdHex != null) {
                    TextButton(onClick = { picking = true }, enabled = state.phase != ReportPhase.SENDING) {
                        Text(t("report.add_messages"))
                    }
                }
            }
            if (line.n == 0) {
                Text(
                    t("report.empty_hint"),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(12.dp))
            // 3. Anything to add.
            OutlinedTextField(
                value = state.comment,
                onValueChange = { state = state.withComment(it) },
                label = { Text(t("report.comment")) },
                minLines = 2,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(8.dp))
            val alsoKey = when (subject.also) {
                is ReportAlso.Block -> t("report.also_block", mapOf("name" to subject.name))
                is ReportAlso.Leave -> t("report.also_leave")
                is ReportAlso.Unsubscribe -> t("report.also_unsubscribe")
                ReportAlso.None -> null
            }
            if (alsoKey != null) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 48.dp)
                        .toggleable(
                            value = state.also,
                            role = Role.Checkbox,
                            onValueChange = { state = state.withAlso(it) },
                        ),
                ) {
                    Checkbox(checked = state.also, onCheckedChange = null)
                    Spacer(Modifier.size(8.dp))
                    Text(alsoKey, style = MaterialTheme.typography.bodyLarge)
                }
            }
            Spacer(Modifier.height(8.dp))
            val whoSees = if (subject.targetKind == me.lakko.app.moderation.ReportTargetKind.PERSON) {
                t(
                    "common.sentence_pair",
                    mapOf(
                        "first" to t("report.who_sees"),
                        "second" to t("report.not_told", mapOf("name" to subject.name)),
                    ),
                )
            } else {
                t("report.who_sees")
            }
            Text(whoSees, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            val errorKey = when (state.phase) {
                ReportPhase.FAILED -> "report.failed"
                ReportPhase.LIMITED -> "report.limited"
                ReportPhase.UNAVAILABLE -> "report.unavailable"
                else -> null
            }
            if (errorKey != null) {
                Spacer(Modifier.height(8.dp))
                Text(t(errorKey), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error)
            }
            Spacer(Modifier.height(12.dp))
            Button(
                onClick = {
                    if (!state.canSend) return@Button
                    state = state.copy(phase = ReportPhase.SENDING)
                    val sending = state
                    scope.launch {
                        val outcome = me.lakko.app.moderation.ModerationClient().use { client ->
                            me.lakko.app.moderation.sendReport(
                                me.lakko.app.moderation.ProductionReportPorts(context, subject, sending, client),
                            )
                        }
                        if (sending.also && me.lakko.app.moderation.alsoApplies(outcome, subject.also)) onAlso(subject.also)
                        val next = me.lakko.app.moderation.phaseAfter(outcome)
                        if (next == null) {
                            val sent = outcome as me.lakko.app.moderation.ReportOutcome.Sent
                            me.lakko.app.moderation.recordSentReport(context, subject, sending, sent.receiptId)
                            onClose()
                            ModerationRequests.showNotice("report.sent")
                        } else {
                            state = sending.copy(phase = next)
                        }
                    }
                },
                enabled = state.canSend,
                modifier = Modifier.fillMaxWidth(),
            ) {
                if (state.phase == ReportPhase.SENDING) {
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                } else {
                    Text(t("report.send"))
                }
            }
        }
    }
}

/**
 * `Add messages` (UX section 2.2): the conversation's messages that may be
 * picked for this target, up to [REPORT_MESSAGE_LIMIT]. Own messages never.
 */
@Composable
private fun ReportMessagePicker(
    subject: ReportSubject,
    chatRepository: ChatRepository,
    initial: List<ReportableMessage>,
    onDone: (List<ReportableMessage>) -> Unit,
) {
    val chat by chatRepository.state.collectAsStateWithLifecycle()
    val groupIdHex = subject.pickGroupIdHex ?: return
    val candidates = remember(chat.threads, subject) {
        chat.threads.firstOrNull { it.groupIdHex == groupIdHex }?.messages.orEmpty()
            .filter { it.direction == Direction.Incoming && !it.deleted && it.call == null }
            .filter { pickable(subject, it.authorKey, isOwn = false) }
            .reversed()
    }
    var picked by remember { mutableStateOf(initial.associateBy { it.messageId }) }
    val atLimit = picked.size >= REPORT_MESSAGE_LIMIT
    Column(Modifier.fillMaxWidth().heightIn(max = 560.dp)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
        ) {
            Text(t("report.select.title"), style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
            TextButton(onClick = { onDone(picked.values.sortedBy { it.messageId }) }) { Text(t("report.select.done")) }
        }
        if (atLimit) {
            Text(
                t("report.select.limit", emptyMap(), mapOf("n" to REPORT_MESSAGE_LIMIT.toULong())),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
        }
        LazyColumn(Modifier.fillMaxWidth()) {
            items(candidates, key = { it.messageId }) { msg ->
                val selected = msg.messageId in picked
                ListItem(
                    headlineContent = { Text(msg.text.ifBlank { " " }, maxLines = 3) },
                    leadingContent = { Checkbox(checked = selected, onCheckedChange = null) },
                    colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
                    modifier = Modifier.toggleable(
                        value = selected,
                        enabled = selected || !atLimit,
                        role = Role.Checkbox,
                        onValueChange = { on ->
                            picked = if (on) {
                                picked + (msg.messageId to ReportableMessage(msg.messageId, groupIdHex, msg.evidence))
                            } else {
                                picked - msg.messageId
                            }
                        },
                    ),
                )
            }
        }
    }
}

/**
 * `Add messages` on a channel-info report (UX section 2.1): the channel's
 * stored posts, newest first, up to [REPORT_MESSAGE_LIMIT]. Every post in a
 * channel is a publisher's, so none is the reporter's own.
 */
@Composable
private fun ReportPostPicker(
    channelIdHex: String,
    channelState: me.lakko.app.channels.ChannelsUiState?,
    initial: List<me.lakko.app.moderation.ReportablePost>,
    onDone: (List<me.lakko.app.moderation.ReportablePost>) -> Unit,
) {
    val context = LocalContext.current.applicationContext
    val inMemory = channelState?.postsByChannel?.collectAsStateWithLifecycle()?.value?.get(channelIdHex).orEmpty()
    val account = channelState?.boundAccount ?: BlockRegister.state.value?.accountIdB64
    var logged by remember(channelIdHex, account) {
        mutableStateOf<List<me.lakko.app.feed.PersistedChannelPost>>(emptyList())
    }
    LaunchedEffect(channelIdHex, account) {
        if (account == null) return@LaunchedEffect
        logged = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            me.lakko.app.feed.FeedSealedStore.loadChannelPostLog(context, account)
                .channels[channelIdHex.lowercase()]?.posts.orEmpty()
        }
    }
    val candidates = remember(logged, inMemory) {
        me.lakko.app.moderation.channelPostCandidates(
            logged = logged,
            inMemoryWireB64 = inMemory.associate { it.cursor.toLong() to it.payloadB64 },
        )
    }
    var picked by remember { mutableStateOf(initial.associateBy { it.cursor }) }
    val atLimit = picked.size >= REPORT_MESSAGE_LIMIT
    Column(Modifier.fillMaxWidth().heightIn(max = 560.dp)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
        ) {
            Text(t("report.select.title"), style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
            TextButton(onClick = { onDone(picked.values.sortedByDescending { it.cursor }) }) { Text(t("report.select.done")) }
        }
        if (atLimit) {
            Text(
                t("report.select.limit", emptyMap(), mapOf("n" to REPORT_MESSAGE_LIMIT.toULong())),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
        }
        LazyColumn(Modifier.fillMaxWidth()) {
            items(candidates, key = { it.cursor }) { post ->
                val selected = post.cursor in picked
                ListItem(
                    headlineContent = { Text(post.text.ifBlank { " " }, maxLines = 3) },
                    leadingContent = { Checkbox(checked = selected, onCheckedChange = null) },
                    colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
                    modifier = Modifier.toggleable(
                        value = selected,
                        enabled = selected || !atLimit,
                        role = Role.Checkbox,
                        onValueChange = { on -> picked = if (on) picked + (post.cursor to post) else picked - post.cursor },
                    ),
                )
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Settings > Privacy
// -----------------------------------------------------------------------------

/** Settings > Privacy > Blocked people (UX section 1.1). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun BlockedPeopleScreen(contactsRepository: ContactsRepository, onBack: () -> Unit) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current.applicationContext
    val directory = remember(context) { me.lakko.app.profile.ProfileDirectory.get(context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val register by BlockRegister.state.collectAsStateWithLifecycle()
    val blocked = register?.blocked.orEmpty()
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(t("block.list.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        if (blocked.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
                Text(
                    t("block.list.empty"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            return@Scaffold
        }
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            items(blocked, key = { it.accountKeyB64 }) { person ->
                val name = nameVersion.let { directory.resolve(person.accountKeyB64) }
                ListItem(
                    headlineContent = { Text(name.primary) },
                    trailingContent = {
                        TextButton(onClick = {
                            scope.launch { applyBlock(contactsRepository, person.accountKeyB64, blocked = false) }
                        }) { Text(t("block.unblock")) }
                    },
                )
            }
        }
    }
}

/** Settings > Privacy > Your reports (UX section 2.3); this device only. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun YourReportsScreen(onBack: () -> Unit) {
    val context = LocalContext.current.applicationContext
    // The signed-in account, as the synced state names it.
    val accountIdB64 = BlockRegister.state.collectAsStateWithLifecycle().value?.accountIdB64
    val reports = remember(accountIdB64) {
        accountIdB64?.let { me.lakko.app.moderation.SharedPrefsSentReportStore(context).load(it) }.orEmpty()
    }
    var statuses by remember { mutableStateOf<Map<String, me.lakko.app.moderation.ReportStatus>>(emptyMap()) }
    // The status is fetched with the receipt and nothing else, when the list opens.
    LaunchedEffect(reports) {
        if (reports.isEmpty()) return@LaunchedEffect
        me.lakko.app.moderation.ModerationClient().use { client ->
            for (r in reports) {
                val status = client.reportStatus(r.receiptId) ?: continue
                statuses = statuses + (r.receiptId to status)
            }
        }
    }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(t("report.list.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        if (reports.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
                Text(
                    t("report.list.empty"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            return@Scaffold
        }
        LazyColumn(Modifier.fillMaxSize().padding(padding)) {
            items(reports, key = { it.receiptId }) { report ->
                val reason = reasonKey(runCatching { uniffi.lakko_ffi_uniffi.reportReasonName(report.reason.toUByte()) }.getOrNull())
                val date = remember(report.sentAtMs) {
                    java.text.DateFormat.getDateInstance(java.text.DateFormat.MEDIUM).format(java.util.Date(report.sentAtMs))
                }
                ListItem(
                    headlineContent = { Text(report.targetName) },
                    supportingContent = {
                        Text(
                            if (reason != null) {
                                t("common.fact_pair", mapOf("first" to t(reason), "second" to date))
                            } else {
                                date
                            },
                        )
                    },
                    trailingContent = {
                        statuses[report.receiptId]?.let { status ->
                            Text(
                                t(status.key),
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    },
                )
            }
        }
    }
}
