// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.text.format.DateFormat
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ConversationRowKind
import me.lakko.app.chat.ForwardBatch
import me.lakko.app.chat.ForwardOutcome
import me.lakko.app.chat.ForwardSender
import me.lakko.app.chat.PendingForwardOutcome
import me.lakko.app.chat.conversationRowMatchesQuery
import me.lakko.app.chat.orderConversationRows
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.devices.sync.FlagsState
import me.lakko.app.devices.sync.ReadCursorState
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.ProfileDirectory
import java.time.ZoneId
import java.util.Locale
import uniffi.lakko_ffi_uniffi.membershipRefusalRetryMaySucceed

/**
 * ADR-0052 decision 2 / `docs/ux/message-actions.md` section 5.1 — the
 * "Forward to" picker.
 *
 * The same merged list the Chats tab shows, restricted to what a forward can
 * go to: one-to-one chats and groups this device is a LIVE member of, pinned
 * first, newest next, with the search field. Channels are not here (a forward
 * is a chat message), and neither is a conversation whose membership has
 * ended (the core refuses the send). One row per conversation, no
 * multi-select in this wave.
 *
 * Picking a row sends at once — no comment field, no confirmation — and takes
 * the person to that conversation, where the forward is the newest bubble.
 * While the attachments are being re-encrypted and uploaded the list is busy
 * and takes no second pick. Back cancels and sends nothing.
 *
 * The rows are built by the same function the Chats tab uses
 * ([threadConversationRows]), so the two lists cannot drift.
 */
@Composable
public fun ForwardPickerScreen(
    viewModel: ForwardViewModel,
    repository: ChatRepository,
    deviceGroupSync: DeviceGroupSyncRepository?,
    onBack: () -> Unit,
    /** The forward landed in this conversation: open it. */
    onForwarded: (targetGroupIdHex: String) -> Unit,
    modifier: Modifier = Modifier,
    /** Section 8 — a selection's forward stopped short: return to the source
     *  conversation, which reads the outcome and keeps the unsent selected.
     *  Defaults to [onBack], which is what the picker's route does. */
    onReturnToSource: () -> Unit = onBack,
) {
    // A picker with nothing to forward — a process restored onto this route —
    // closes rather than showing a list that can do nothing.
    if (viewModel.batch == null) {
        LaunchedEffect(Unit) { onBack() }
        return
    }
    val busy by viewModel.busy.collectAsStateWithLifecycle()
    val error by viewModel.error.collectAsStateWithLifecycle()
    ConversationPicker(
        // Section 5.1 — "the picker's title is 'Forward to'".
        title = t(FORWARD_PICKER_TITLE),
        repository = repository,
        deviceGroupSync = deviceGroupSync,
        busy = busy,
        // The attachments are being re-encrypted and uploaded under fresh
        // keys (ADR-0052 OQ1); the list waits for the send.
        busyLabel = t(FORWARDING_LABEL),
        error = error,
        onBack = onBack,
        onPick = { row ->
            viewModel.forward(
                targetGroupIdHex = row.id,
                onDone = { onForwarded(row.id) },
                onReturnToSource = onReturnToSource,
            )
        },
        modifier = modifier,
    )
}

/**
 * The list of conversations a message can be sent into: the body of the
 * "Forward to" picker, and of the call screen's "Invite to call"
 * (`docs/ux/call-links.md` section 3: "the same list `Forward to` shows").
 *
 * The same merged list the Chats tab shows, restricted to one-to-one chats
 * and groups this device is a LIVE member of, pinned first, newest next, with
 * the search field. The rows are built by the same function the Chats tab
 * uses ([threadConversationRows]), so the lists cannot drift.
 *
 * While [busy] the list takes no pick and [busyLabel] names the wait to a
 * screen reader; [error] is one sentence above the list. A row in [doneIds]
 * has already been sent to: it shows a check and [doneLabel] and takes no
 * second pick, which is how the call's picker lets a person invite several
 * conversations one after another without leaving it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ConversationPicker(
    title: String,
    repository: ChatRepository,
    deviceGroupSync: DeviceGroupSyncRepository?,
    busy: Boolean,
    busyLabel: String,
    error: UiText?,
    onBack: () -> Unit,
    onPick: (me.lakko.app.chat.ConversationListRow) -> Unit,
    modifier: Modifier = Modifier,
    doneIds: Set<String> = emptySet(),
    doneLabel: String = "",
) {
    val state by repository.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val directory = remember(repository) { ProfileDirectory.get(repository.context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val groupNameVersion by me.lakko.app.chat.GroupNameStore.version.collectAsStateWithLifecycle()
    val flagsState = deviceGroupSync?.flags?.collectAsStateWithLifecycle()
    val flagsMap: Map<String, FlagsState> = flagsState?.value ?: emptyMap()
    val readCursorState = deviceGroupSync?.readCursors?.collectAsStateWithLifecycle()
    val readCursorMap: Map<String, ReadCursorState> = readCursorState?.value ?: emptyMap()
    var query by rememberSaveable { mutableStateOf("") }

    val zone = remember { ZoneId.systemDefault() }
    val strings = LocalStrings.current
    val locale = remember(strings.language) { Locale.forLanguageTag(strings.language) }
    val use24h = remember(context) { DateFormat.is24HourFormat(context) }

    val rows = remember(state, nameVersion, groupNameVersion, flagsMap, readCursorMap, strings) {
        orderConversationRows(
            threadConversationRows(
                context = context,
                repository = repository,
                directory = directory,
                // Live memberships only: a pick is a send, and the core
                // refuses one into a group this device has left or been
                // removed from.
                threads = state.threads.filter { !it.membershipEnded },
                flagsMap = flagsMap,
                readCursorMap = readCursorMap,
                nowMs = System.currentTimeMillis(),
                zone = zone,
                use24h = use24h,
                locale = locale,
                strings = strings,
            ),
        )
    }
    val visible = remember(rows, query) {
        rows.filter { conversationRowMatchesQuery(query, it.title, it.preview) }
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(title) },
                navigationIcon = {
                    IconButton(onClick = onBack, enabled = !busy) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
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
            if (busy) {
                LinearProgressIndicator(
                    modifier = Modifier
                        .fillMaxWidth()
                        .semantics { contentDescription = busyLabel },
                )
            }
            error?.let { message ->
                Text(
                    text = message.text(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
                )
            }
            if (visible.isEmpty()) {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text(
                        text = t(if (query.isBlank()) "chat.forward.empty" else "chats.search.no_results"),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            } else {
                LazyColumn(
                    modifier = Modifier
                        .fillMaxSize()
                        .alpha(if (busy) 0.6f else 1f),
                    contentPadding = PaddingValues(vertical = 4.dp),
                ) {
                    items(items = visible, key = { "${it.kind}:${it.id}" }) { row ->
                        // The list only ever holds chats and groups; the kind
                        // is checked so a channel row could never be picked
                        // as a send target even if one were mapped in later.
                        val done = row.id in doneIds
                        val pickable = !busy && !done && row.kind != ConversationRowKind.CHANNEL
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            modifier = Modifier.clickable(enabled = pickable) { onPick(row) },
                        ) {
                            ConversationRowBody(row = row, modifier = Modifier.weight(1f))
                            if (done) {
                                Row(
                                    verticalAlignment = Alignment.CenterVertically,
                                    modifier = Modifier
                                        .padding(end = 16.dp)
                                        .semantics(mergeDescendants = true) { contentDescription = doneLabel },
                                ) {
                                    Icon(
                                        Icons.Filled.Check,
                                        contentDescription = null,
                                        tint = MaterialTheme.colorScheme.primary,
                                    )
                                    Text(
                                        text = doneLabel,
                                        style = MaterialTheme.typography.labelMedium,
                                        color = MaterialTheme.colorScheme.primary,
                                        modifier = Modifier.padding(start = 4.dp),
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/** Section 5.1's title for the picker. */
internal const val FORWARD_PICKER_TITLE: String = "chat.forward.picker_title"

/** What the progress line says while the forward is in flight. */
internal const val FORWARDING_LABEL: String = "common.sending"

/**
 * The picker's state: the batch it was opened with (taken once from the
 * [me.lakko.app.chat.PendingForward] slot, so it survives rotation with the
 * view model rather than being spent by it), the in-flight flag, and the
 * last failure.
 */
public class ForwardViewModel(
    private val repo: ChatRepository,
    /** `null` when the slot was empty on arrival — the picker then closes. */
    public val batch: ForwardBatch?,
    private val sender: ForwardSender = ForwardSender(
        send = { target, text, attachments, forward ->
            repo.sendMessage(
                groupIdHex = target,
                plaintext = text,
                attachments = attachments,
                forward = forward,
            )
        },
    ),
    /** Section 8 — where a selection's outcome is left for the source. */
    private val publishOutcome: (ForwardOutcome) -> Unit = PendingForwardOutcome::publish,
) : ViewModel() {
    private val mutableBusy = MutableStateFlow(false)
    private val mutableError = MutableStateFlow<UiText?>(null)

    /** `true` from the pick until the send lands or fails. */
    public val busy: StateFlow<Boolean> = mutableBusy.asStateFlow()

    /** One sentence when the last forward failed, else `null`. */
    public val error: StateFlow<UiText?> = mutableError.asStateFlow()

    /**
     * Send the batch into [targetGroupIdHex], oldest first, stopping at the
     * first failure. On full success [onDone] runs. A failed single forward
     * (section 5.1) is reported here, in the picker; a failed selection
     * (section 8) leaves the outcome for the source conversation and runs
     * [onReturnToSource], so the person lands back on the selection with the
     * unsent messages still selected and "Forwarded K of N." A second pick
     * while one is in flight is ignored — the list is disabled meanwhile, so
     * this is the seam's own guard rather than the UI's.
     */
    public fun forward(
        targetGroupIdHex: String,
        onDone: () -> Unit,
        onReturnToSource: () -> Unit = {},
    ) {
        val current = batch ?: return
        if (!mutableBusy.compareAndSet(expect = false, update = true)) return
        mutableError.value = null
        viewModelScope.launch {
            val result = try {
                sender.forwardAll(current.requests, targetGroupIdHex)
            } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                mutableBusy.value = false
                throw e
            }
            mutableBusy.value = false
            if (current.fromSelection) {
                // The source reads this whether the run completed or not: a
                // complete outcome is what tells it to leave selection mode.
                publishOutcome(
                    ForwardOutcome(
                        sourceGroupIdHex = current.sourceGroupIdHex,
                        done = result.done,
                        total = result.total,
                        unsentMessageIds = result.unsent.map { it.message.messageId }.toSet(),
                    ),
                )
            }
            when {
                result.complete -> onDone()
                current.fromSelection -> onReturnToSource()
                else -> {
                    // A refusal that names itself keeps its own words, as the
                    // composer's failure line does.
                    val failure = result.failure
                    if (failure != null) android.util.Log.w("ForwardViewModel", "forward failed", failure)
                    mutableError.value = if (failure == null) {
                        UiText("chat.forward.error.generic")
                    } else {
                        me.lakko.app.chat.chatErrorText(
                            failure,
                            "chat.forward.error.generic",
                            ::membershipRefusalRetryMaySucceed,
                        )
                    }
                }
            }
        }
    }
}
