// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.chat.AttachmentBridgeException
import me.lakko.app.chat.UploadStage
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ChatThread
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * State + dispatcher for the per-thread [ChatScreen].
 *
 * Combines the [ChatRepository.state]-side thread (looked up by
 * [groupIdHex]) with VM-local composer state (the current
 * draft input + the send-busy / send-error flags) into a
 * single [ChatScreenUiState] flow.
 *
 * Manual refresh only — the polling loop lands in 1.15e7 (the
 * same sub-sprint that wires background polling for the
 * contacts inbox).
 */
public class ChatViewModel(
    private val repo: ChatRepository,
    public val groupIdHex: String,
) : ViewModel() {

    private val composer: MutableStateFlow<ComposerState> =
        MutableStateFlow(ComposerState())

    private val derived: MutableStateFlow<ChatScreenUiState> =
        MutableStateFlow(buildState(repo.state.value, composer.value))

    public val state: StateFlow<ChatScreenUiState> = derived.asStateFlow()

    init {
        // Manually combine two StateFlows — kotlinx-coroutines
        // has no synchronous `combine` for two StateFlows;
        // two parallel collectors keep the derived flow hot
        // for the ViewModel's lifetime.
        viewModelScope.launch {
            repo.state.collect { repoState ->
                derived.value = buildState(repoState, composer.value)
            }
        }
        viewModelScope.launch {
            composer.collect { composerState ->
                derived.value = buildState(repo.state.value, composerState)
            }
        }
    }

    // -------------------------------------------------------------
    // Phase 2a3e2b-Android — cross-device draft sync
    // -------------------------------------------------------------

    /** Attached via [attachSync] from the screen. `null` until
     *  the screen lifecycle wires it in. */
    private var deviceGroupSync: DeviceGroupSyncRepository? = null
    /** Raw group_id bytes for the active thread — cached so
     *  the broadcast / seed paths don't re-decode the hex
     *  string on every keystroke. */
    private var groupIdBytes: ByteArray? = null
    /** Last draft text the broadcast path published — dedups
     *  echoes that loop back through the poll loop. */
    private var lastBroadcastDraft: String? = null
    private var draftBroadcastJob: Job? = null
    private var didSeedDraft: Boolean = false
    private val DRAFT_DEBOUNCE_MS: Long = 500L

    /**
     * Bind to the process-scoped [DeviceGroupSyncRepository]
     * (Phase 2a3e2b). Called from the Compose ChatScreen on
     * `LaunchedEffect(Unit)`. Subscribes to:
     *
     *   * incoming `Draft` payloads keyed on this thread —
     *     seed the composer once on first mount when local
     *     is empty (avoids stomping mid-typing on this
     *     device).
     *   * local `draft` changes — debounce 500 ms then
     *     broadcast.
     *
     * Idempotent: a second call replaces the prior reference
     * but the in-flight collectors keep running.
     */
    public fun attachSync(sync: DeviceGroupSyncRepository) {
        if (deviceGroupSync === sync) return
        deviceGroupSync = sync
        val gid = hexToBytes(groupIdHex) ?: return
        groupIdBytes = gid

        viewModelScope.launch {
            sync.drafts.collect { drafts ->
                if (didSeedDraft) return@collect
                val incoming = drafts[groupIdHex] ?: return@collect
                if (composer.value.draft.isEmpty()) {
                    composer.update { it.copy(draft = incoming.text) }
                }
                didSeedDraft = true
            }
        }

        viewModelScope.launch {
            composer
                .asStateFlow()
                .distinctUntilChanged { a, b -> a.draft == b.draft }
                .collect { snapshot ->
                    scheduleDraftBroadcast(snapshot.draft, gid)
                }
        }
    }

    private fun scheduleDraftBroadcast(text: String, gid: ByteArray) {
        if (text == lastBroadcastDraft) return
        draftBroadcastJob?.cancel()
        draftBroadcastJob = viewModelScope.launch {
            delay(DRAFT_DEBOUNCE_MS)
            lastBroadcastDraft = text
            try {
                deviceGroupSync?.setDraft(groupId = gid, text = text)
            } catch (cause: Throwable) {
                // Best-effort — the next keystroke retries.
                android.util.Log.w(
                    "ChatViewModel",
                    "setDraft failed",
                    cause,
                )
            }
        }
    }

    private fun hexToBytes(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        val out = ByteArray(hex.length / 2)
        var i = 0
        while (i < hex.length) {
            val hi = Character.digit(hex[i], 16)
            val lo = Character.digit(hex[i + 1], 16)
            if (hi < 0 || lo < 0) return null
            out[i / 2] = ((hi shl 4) or lo).toByte()
            i += 2
        }
        return out
    }

    private fun buildState(
        repoState: me.lakko.app.chat.ChatRepositoryState,
        composer: ComposerState,
    ): ChatScreenUiState {
        val thread: ChatThread? =
            repoState.threads.firstOrNull { it.groupIdHex == groupIdHex }
        return ChatScreenUiState(
            groupIdHex = groupIdHex,
            messages = thread?.messages ?: emptyList(),
            draft = composer.draft,
            sending = composer.sending,
            sendError = composer.sendError,
            refreshing = composer.refreshing,
            pendingAttachments = composer.pendingAttachments,
            uploading = composer.uploading,
            uploadStage = composer.uploadStage,
            // We always render the screen as "exists" — if the
            // repo doesn't carry this thread (no Group handle
            // built yet, post-restart pre-bootstrap), the
            // messages list is just empty and the composer
            // still works (sendMessage will throw with a clear
            // error which we surface as `sendError`).
            threadKnown = thread != null,
        )
    }

    public fun onDraftChange(value: String) {
        composer.update { it.copy(draft = value) }
    }

    public fun onSendClicked() {
        val snapshot = composer.value
        if (snapshot.sending) return
        val trimmed = snapshot.draft.trim()
        val stagedAttachments = snapshot.pendingAttachments
        // 1.19c4: attachment-only messages are valid (empty
        // text + ≥1 attachment); pre-1.19 text-only messages
        // still valid (≥1 char text + zero attachments). The
        // only invalid input is the all-empty composer.
        if (trimmed.isEmpty() && stagedAttachments.isEmpty()) return

        composer.update { it.copy(sending = true, sendError = null) }
        viewModelScope.launch {
            try {
                repo.sendMessage(
                    groupIdHex = groupIdHex,
                    plaintext = trimmed,
                    attachments = stagedAttachments,
                )
                composer.update {
                    it.copy(
                        sending = false,
                        sendError = null,
                        draft = "",
                        pendingAttachments = emptyList(),
                    )
                }
            } catch (err: Throwable) {
                // 1.19c4: leave `pendingAttachments` populated
                // on send failure so the user can retry without
                // re-picking the same files.
                composer.update {
                    it.copy(
                        sending = false,
                        sendError = err.message ?: "Send failed.",
                    )
                }
            }
        }
    }

    /**
     * Drive the encrypt + upload flow for one picked item.
     * Called by the screen layer after the OS-level picker
     * resolves a URI to raw bytes + MIME + display name.
     * Sequential — one item at a time — keeps the
     * upload-state flag honest.
     */
    public fun onAttachmentPicked(bytes: ByteArray, mime: String?, name: String?) {
        val displayName = name?.takeUnless { it.isNullOrEmpty() } ?: "attachment"
        composer.update {
            it.copy(
                uploading = true,
                sendError = null,
                // Polish 1.x — drive `uploadStage` immediately
                // so the composer doesn't show a generic spinner
                // for the brief encrypt window. The
                // `AttachmentBridge.encryptAndUpload` callback
                // flips this to `Uploading` once it reaches the
                // PUT path.
                uploadStage = UploadProgress(
                    fileName = displayName,
                    stage = UploadStage.Encrypting,
                ),
            )
        }
        viewModelScope.launch {
            try {
                val ref = AttachmentBridge.encryptAndUpload(
                    bytes = bytes,
                    mime = mime,
                    name = name,
                    onStage = { stage ->
                        composer.update {
                            it.copy(
                                uploadStage = UploadProgress(
                                    fileName = displayName,
                                    stage = stage,
                                ),
                            )
                        }
                    },
                )
                composer.update {
                    it.copy(
                        uploading = false,
                        uploadStage = null,
                        pendingAttachments = it.pendingAttachments + ref,
                    )
                }
            } catch (err: AttachmentBridgeException) {
                composer.update {
                    it.copy(
                        uploading = false,
                        uploadStage = null,
                        sendError = err.message ?: "Attachment failed.",
                    )
                }
            } catch (err: Throwable) {
                composer.update {
                    it.copy(
                        uploading = false,
                        uploadStage = null,
                        sendError = "Attachment failed: ${err.message ?: err}",
                    )
                }
            }
        }
    }

    /** Drop a staged attachment from the composer's pending
     *  tray. Index matches the order in
     *  [ChatScreenUiState.pendingAttachments]. */
    public fun onRemovePendingAttachment(index: Int) {
        composer.update {
            if (index !in it.pendingAttachments.indices) it
            else it.copy(
                pendingAttachments = it.pendingAttachments.toMutableList().apply {
                    removeAt(index)
                },
            )
        }
    }

    public fun onRefreshClicked() {
        composer.update { it.copy(refreshing = true) }
        viewModelScope.launch {
            try {
                repo.pollGroup(groupIdHex)
            } finally {
                composer.update { it.copy(refreshing = false) }
            }
        }
    }

    /**
     * One long-poll tick for the per-thread continuous-polling
     * loop the composable drives via
     * `LaunchedEffect(Unit) { while (isActive) { pollOnce(...) } }`.
     * Silent — does NOT flip the `refreshing` flag (the spinner
     * would otherwise stay on for the full long-poll window).
     */
    public suspend fun pollOnceLongPoll(waitSecs: Int = DEFAULT_LONG_POLL_SECS) {
        repo.pollGroup(groupIdHex, waitSecs = waitSecs)
    }

    public companion object {
        /** DS long-poll budget — same value as
         *  [me.lakko.app.ui.contacts.ContactsViewModel]. */
        public const val DEFAULT_LONG_POLL_SECS: Int = 30
    }

    /** VM-local composer + transient flags. Persistent timeline
     *  data lives in [ChatRepository]. */
    private data class ComposerState(
        val draft: String = "",
        val sending: Boolean = false,
        val sendError: String? = null,
        val refreshing: Boolean = false,
        /** Phase 1.19c4 — attachments staged in the composer.
         *  The user picks a file / photo; each call to
         *  [ChatViewModel.onAttachmentPicked] encrypts +
         *  uploads to storage immediately so the ref is ready
         *  by the time Send is tapped. Cleared on successful
         *  Send, on per-chip remove, and on screen disposal
         *  (ViewModel.onCleared). */
        val pendingAttachments: List<AttachmentRef> = emptyList(),
        /** `true` while one or more picked items are being
         *  encrypted + uploaded. The composer disables Send +
         *  the picker buttons until it flips back. */
        val uploading: Boolean = false,
        /** Polish 1.x — per-pick upload-progress detail.
         *  `null` when no upload is in flight; otherwise carries
         *  the file name + which stage of the encrypt / upload
         *  pipeline the in-flight pick is at. */
        val uploadStage: UploadProgress? = null,
    )
}

/** Polish 1.x — composer-tray progress chip payload.
 *  Mirror of the iOS `ChatViewModel.UploadStage` enum and the
 *  web client's `uploadProgress` signal. */
public data class UploadProgress(
    /** Display name of the file currently being processed. */
    val fileName: String,
    /** Which pipeline stage the bridge is in. */
    val stage: UploadStage,
)

/** Combined snapshot the [ChatScreen] renders. */
public data class ChatScreenUiState(
    val groupIdHex: String,
    val messages: List<ChatMessage>,
    val draft: String,
    val sending: Boolean,
    val sendError: String?,
    val refreshing: Boolean,
    /** 1.19c4 — staged attachments below the text composer. */
    val pendingAttachments: List<AttachmentRef> = emptyList(),
    /** 1.19c4 — `true` while a picked item is being encrypted
     *  + uploaded. */
    val uploading: Boolean = false,
    /** Polish 1.x — per-pick upload-progress detail. `null`
     *  when no upload is in flight. */
    val uploadStage: UploadProgress? = null,
    /** `false` when the repository has no thread for this
     *  groupIdHex — e.g. immediately after process restart
     *  before snapshot rehydration has rebuilt the per-group
     *  handles. The UI still renders the composer (sendMessage
     *  surfaces a clear error if the handle is missing). */
    val threadKnown: Boolean,
)
