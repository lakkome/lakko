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
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.chat.AttachmentBridgeException
import me.lakko.app.chat.UploadStage
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ChatThread
import me.lakko.app.calls.CallJwtClient
import me.lakko.app.calls.CallSession
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.SfuCallSession
import me.lakko.app.calls.SfuCallUi
import me.lakko.app.calls.SfuConfig
import me.lakko.app.calls.parseCallSignalingBytes
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.contacts.AcceptedContactsStore
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import uniffi.lakko_ffi_uniffi.GroupMember
import uniffi.lakko_ffi_uniffi.Role
import uniffi.lakko_ffi_uniffi.RoleTable
import uniffi.lakko_ffi_uniffi.sfuPeerTag

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
    /** SFU group-call admission needs the account's session bearer to
     *  mint a call JWT. Threaded in from `MainActivity` (where the
     *  `AuthRepository` lives) as a closure so the VM has no auth-layer
     *  dependency. Returns `null` when signed out — `startGroupCall`
     *  then refuses with a clear log line. Defaults to `{ null }` so
     *  existing call sites + tests compile unchanged. */
    private val sessionTokenProvider: () -> String? = { null },
    /** Resolves `@username -> (accountId, signaturePubkey)` through the
     *  G-4-verified contacts path for the add-member flow (A4). Threaded
     *  from `MainActivity` as `{ contactsRepository.resolveVerifiedRecipient(it) }`.
     *  Defaults to a fail-closed stub so existing call sites + tests compile
     *  unchanged (add-member then refuses). */
    private val resolveRecipient: suspend (String) -> Pair<ByteArray, ByteArray> = {
        throw IllegalStateException("no @username resolver wired — add-member unavailable")
    },
) : ViewModel() {

    private val composer: MutableStateFlow<ComposerState> =
        MutableStateFlow(ComposerState())

    private val derived: MutableStateFlow<ChatScreenUiState> =
        MutableStateFlow(buildState(repo.state.value, composer.value))

    public val state: StateFlow<ChatScreenUiState> = derived.asStateFlow()

    // -------------------------------------------------------------
    // Phase 3a10 — call surface
    // -------------------------------------------------------------

    private val mutableActiveCall: MutableStateFlow<CallSession?> = MutableStateFlow(null)

    /** Active 1×1 call for this thread, or `null`. UI
     *  collects this StateFlow to render the call overlay. */
    public val activeCall: StateFlow<CallSession?> = mutableActiveCall.asStateFlow()

    init {
        // ADR-0042 — chat-open push-routing provisioning (mirror of the web
        // Chat.tsx onMount `provisionPushRouting`): owner + no secret ->
        // mint + persist + distribute + subscribe under the opaque
        // `push_gid`; member with a secret -> subscribe; else defer.
        // Idempotent + race-safe; fire-and-forget inside the repository.
        repo.ensurePushRouting(groupIdHex)
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
        // ADR-0024 — read-receipt SEND. Each time the thread's
        // highest received messageId advances past our own
        // last-published watermark, publish a throttled
        // `read_up_to` so peers learn we have read up to it.
        // Distinct-on-the-highest-id so a no-op repo emission
        // (e.g. a typing fold) doesn't re-trigger.
        viewModelScope.launch {
            repo.state
                .distinctUntilChanged { a, b ->
                    highestMessageIdFor(a) == highestMessageIdFor(b)
                }
                .collect { repoState ->
                    scheduleReadReceipt(highestMessageIdFor(repoState))
                }
        }
        // G-18 / ADR-0029 — mid-call call-key rotation. While an SFU group
        // call is live, re-derive the per-call seed on each repo emission
        // (an emission follows every poll that may have applied an
        // epoch-advancing commit — add / remove / update) and rekey the
        // call. `SfuCallUi.rekey` no-ops when the seed is unchanged, so this
        // only rotates on an actual epoch advance; the decryptors keep the
        // previous key for the overlap window so in-flight frames still
        // open, then drop it (forward secrecy). Fail-closed: a null
        // derivation keeps the current seed — never a downgrade (invariant
        // #4). The body returns immediately when no SFU call is live, so
        // there is no exporter cost outside a call.
        viewModelScope.launch {
            repo.state.collect {
                val handle = mutableActiveSfuCall.value ?: return@collect
                val callId = activeSfuCallId ?: return@collect
                val key = repo.deriveCallKey(groupIdHex = groupIdHex, callId = callId)
                if (key != null) {
                    handle.rekey(key.seed)
                }
            }
        }
        // ADR-0032 piece B — mid-call camera-on bridge for the SFU group call.
        // The store-driven `CallScreen` is presented WITHOUT a session (only
        // `sfuCallUiState`), so the camera button only flips the store's local
        // `camOn`; mirror it onto the live SFU session here (the analog of the
        // web `Chat.tsx` camera effect). camOn TRUE -> `publishVideo` (acquire
        // the camera + SFrame-seal + renegotiate); FALSE -> disable the track.
        // On a publishVideo failure revert the toggle — fail-safe, the call
        // stays up audio-only (invariant #4; publishVideo never leaves a
        // half-added plaintext sender behind). `publishVideo` is idempotent +
        // in-flight-guarded, so a redundant emission is a cheap no-op.
        viewModelScope.launch {
            sfuCallUiState.participants
                .map { list -> list.firstOrNull { it.isLocal }?.camOn ?: false }
                .distinctUntilChanged()
                .collect { camOn ->
                    val handle = mutableActiveSfuCall.value ?: return@collect
                    if (camOn) {
                        try {
                            handle.publishVideo()
                        } catch (err: Throwable) {
                            sfuCallUiState.setCamera(false)
                            android.util.Log.w(
                                "ChatViewModel",
                                "SFU mid-call publishVideo failed — reverting camera",
                                err,
                            )
                        }
                    } else {
                        handle.setCameraEnabled(false)
                    }
                }
        }
        // Phase 3a10 — attach a call-signaling sink to the
        // repository. The closure runs on whatever
        // dispatcher the per-record poll loop uses; we hop
        // back through the ViewModel scope to mutate
        // StateFlow safely.
        repo.callSignalingSink = { hex, payloadBytes ->
            if (hex == groupIdHex) {
                viewModelScope.launch {
                    handleIncomingCallSignaling(payloadBytes)
                }
            }
        }
    }

    /** Tap-to-call from the chat header. G3-2-prejoin-join: this
     *  now only PREPARES the session — it builds it and arms the
     *  SFrame key, but does NOT dial. The session sits `Idle`
     *  (mapped to the prejoin phase), the call screen presents the
     *  prejoin (mic / camera preview), and the Join button calls
     *  [dialActiveCall]. */
    public fun startCall(audio: Boolean = true, video: Boolean = false) {
        if (mutableActiveCall.value != null) return
        @Suppress("UNUSED_PARAMETER") val ignored = audio to video
        val callId = CallSession.mintCallId()
        val session = makeSession(callId)
        mutableActiveCall.value = session
        // G3-2-call-key — arm the SFrame seed from the MLS exporter
        // so it is ready by the time the user taps Join. Without it
        // dial refuses per invariant #4 (no non-E2EE fallback).
        viewModelScope.launch {
            armCallKey(session, callId)
        }
    }

    /** G3-2-prejoin-join — dial the prepped session from the
     *  prejoin Join button, carrying the user's mic / camera
     *  choice. Re-arms the key first (idempotent) so dial never
     *  races ahead of the seed. No-op if the session already
     *  left `Idle`. */
    public fun dialActiveCall(audio: Boolean = true, video: Boolean = false) {
        val session = mutableActiveCall.value ?: return
        if (session.state.value != CallSession.WebCallState.Idle) return
        viewModelScope.launch {
            armCallKey(session, session.callId)
            session.dial(audio = audio, video = video)
        }
    }

    /** G3-2-prejoin-join — abandon a prepped-but-undialed call
     *  from the prejoin Cancel button: hang up the `Idle` session
     *  and clear it immediately (no "Call ended" overlay for a
     *  call that never connected). */
    public fun cancelActiveCall() {
        val session = mutableActiveCall.value ?: return
        session.hangUp()
        mutableActiveCall.value = null
    }

    /** G3-2-call-key — derive + set the per-call SFrame seed
     *  from the chat group's MLS exporter. Shared by the
     *  outgoing + incoming paths. A failed derivation leaves
     *  the seed unset, which is the correct fail-closed outcome
     *  (the call refuses per security invariant #4). */
    private suspend fun armCallKey(session: CallSession, callId: ByteArray) {
        val key = repo.deriveCallKey(groupIdHex = groupIdHex, callId = callId)
        if (key != null) {
            session.setCallKey(key.seed, key.localLeaf)
        } else {
            android.util.Log.w(
                "ChatViewModel",
                "call-key derivation failed — call will refuse per invariant #4",
            )
        }
    }

    public fun acceptIncomingCall() {
        mutableActiveCall.value?.acceptCall()
    }

    public fun declineIncomingCall() {
        mutableActiveCall.value?.declineCall()
    }

    public fun hangUpCall() {
        mutableActiveCall.value?.hangUp()
    }

    public fun clearFinishedCall() {
        val cur = mutableActiveCall.value ?: return
        if (cur.state.value == CallSession.WebCallState.Done) {
            mutableActiveCall.value = null
        }
    }

    // -------------------------------------------------------------
    // SFU multi-party group call (>2 members)
    // -------------------------------------------------------------

    private val mutableActiveSfuCall: MutableStateFlow<SfuCallUi?> =
        MutableStateFlow(null)

    /** Active multi-party SFU group call for this thread, or `null`. A
     *  group chat (>2 members) routes through this; a 1x1 chat stays on
     *  [activeCall]. The UI presents the call overlay (driven by
     *  [sfuCallUiState]) while this is non-null. */
    public val activeSfuCall: StateFlow<SfuCallUi?> = mutableActiveSfuCall.asStateFlow()

    /** The [CallUiState] the active SFU call drives (roster / phase /
     *  E2EE). The Compose overlay observes this while [activeSfuCall] is
     *  non-null. Reused across calls. */
    public val sfuCallUiState: CallUiState = CallUiState()

    /** G-18 / ADR-0029 — the raw call-id of the active SFU group call, held
     *  so the mid-call rekey trigger can re-derive the per-call seed at the
     *  new MLS epoch (`exportSecret("lakko/call/v1", callId, 32)`). `null`
     *  when no SFU call is live. Read only while [activeSfuCall] is
     *  non-null. */
    private var activeSfuCallId: ByteArray? = null

    /** `true` when this chat has >2 MLS members — the call button then
     *  routes through the SFU group-call path; a 2-member chat stays on
     *  the 1x1 P2P [CallSession]. A `null` member count (no handle yet)
     *  reads as a 1x1. */
    public val isGroupChat: Boolean
        get() = (repo.memberCount(groupIdHex) ?: 0) > 2

    // --- Group membership (A4) ---

    private val mutableMembers: MutableStateFlow<List<GroupMember>> =
        MutableStateFlow(emptyList())

    /** Live MLS roster for this group at the current epoch. */
    public val members: StateFlow<List<GroupMember>> = mutableMembers.asStateFlow()

    private val mutableMembersPanelOpen: MutableStateFlow<Boolean> =
        MutableStateFlow(false)

    /** `true` while the members sheet is shown. */
    public val membersPanelOpen: StateFlow<Boolean> = mutableMembersPanelOpen.asStateFlow()

    private val mutableAddMemberInput: MutableStateFlow<String> = MutableStateFlow("")

    /** `@username` typed into the add-member field. */
    public val addMemberInput: StateFlow<String> = mutableAddMemberInput.asStateFlow()

    private val mutableMembershipBusy: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** `true` while an add / remove commit is in flight. */
    public val membershipBusy: StateFlow<Boolean> = mutableMembershipBusy.asStateFlow()

    private val mutableMembershipError: MutableStateFlow<String?> = MutableStateFlow(null)

    /** Last add / remove error surfaced to the sheet, else `null`. */
    public val membershipError: StateFlow<String?> = mutableMembershipError.asStateFlow()

    private val mutableCanManageMembers: MutableStateFlow<Boolean> = MutableStateFlow(true)

    /** ADR-0022 — `true` when the local member may add / remove (owner, admin,
     *  or an open/legacy group with no role table); `false` for a plain member.
     *  The sheet hides the add field + per-member Remove when this is `false`. */
    public val canManageMembers: StateFlow<Boolean> = mutableCanManageMembers.asStateFlow()

    private val mutableIsOwner: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** ADR-0022 — `true` when the local member is the owner (only the owner sees
     *  the per-member promote / demote toggle). */
    public val isOwner: StateFlow<Boolean> = mutableIsOwner.asStateFlow()

    private val mutableRoleTable: MutableStateFlow<RoleTable?> = MutableStateFlow(null)

    /** The group's current role table (owner + admins), or `null` for an open
     *  group — the members sheet reads it reactively to label + gate the
     *  per-member promote / demote toggle. */
    public val roleTable: StateFlow<RoleTable?> = mutableRoleTable.asStateFlow()

    private val mutablePendingJoins: MutableStateFlow<List<ByteArray>> =
        MutableStateFlow(emptyList())

    /** ADR-0027 (G-3) — pending external-join requests. Each entry is a joiner
     *  leaf signature key (the `proposalRef` for approve / reject). The members
     *  sheet renders an owner/admin-only section from this; empty hides it. The
     *  MLS-core retained set (`group.pendingExternalJoins()`) is the source of
     *  truth — re-read on [refreshMembers] and on each poll while the sheet is
     *  open. */
    public val pendingJoins: StateFlow<List<ByteArray>> = mutablePendingJoins.asStateFlow()

    /** ADR-0028 OQ2 (G-3) — advisory attribution for a pending request: which
     *  member minted the `/g/` link it arrived on, and whether that issuer is in
     *  the current roster. */
    public data class JoinAttributionUi(
        val issuerFingerprint: String,
        val issuerInRoster: Boolean,
    )

    private val mutableJoinAttributions:
        MutableStateFlow<Map<String, JoinAttributionUi>> = MutableStateFlow(emptyMap())

    /** ADR-0028 OQ2 (G-3) — keyed by joiner signature key (full hex). Refreshed
     *  alongside [pendingJoins]; a missing entry means no attribution was posted. */
    public val joinAttributions: StateFlow<Map<String, JoinAttributionUi>> =
        mutableJoinAttributions.asStateFlow()

    private val mutableInviteLink: MutableStateFlow<String?> = MutableStateFlow(null)

    /** ADR-0028 (G-3) — the minted group invite-link (the owner/admin "create
     *  invite link" affordance), or `null` before it is generated. */
    public val inviteLink: StateFlow<String?> = mutableInviteLink.asStateFlow()

    /** Update the add-member text field. */
    public fun setAddMemberInput(value: String) {
        mutableAddMemberInput.value = value
    }

    /** Open / close the members sheet; refresh the roster on open. */
    public fun toggleMembersPanel() {
        val opening = !mutableMembersPanelOpen.value
        mutableMembersPanelOpen.value = opening
        if (opening) {
            mutableMembershipError.value = null
            refreshMembers()
        }
    }

    /** Re-read the live MLS roster from the repository. */
    public fun refreshMembers() {
        mutableMembers.value = repo.members(groupIdHex) ?: emptyList()
        val role = repo.ownRole(groupIdHex)
        mutableCanManageMembers.value = role != Role.MEMBER
        mutableIsOwner.value = role == Role.OWNER
        mutableRoleTable.value = repo.groupRoles(groupIdHex)
        // ADR-0027 (G-3) — refresh the pending external-join list alongside the
        // roster (the retained set is the source of truth).
        mutablePendingJoins.value = repo.pendingExternalJoins(groupIdHex)
        refreshJoinAttributions()
    }

    /** ADR-0028 OQ2 (G-3) — refresh the advisory join-attribution map for the
     *  current pending requests. Best-effort; owner/admin only (the role-gated
     *  approval gate is the boundary). */
    private fun refreshJoinAttributions() {
        if (!mutableCanManageMembers.value) {
            mutableJoinAttributions.value = emptyMap()
            return
        }
        viewModelScope.launch {
            val attrs = repo.fetchJoinAttributions(groupIdHex)
            mutableJoinAttributions.value = attrs.associate { a ->
                a.joinerSignatureKey.joinToString("") { b -> "%02x".format(b.toInt() and 0xff) } to
                    JoinAttributionUi(a.issuerFingerprint, a.issuerInRoster)
            }
        }
    }

    /** Our own leaf index — the roster marks it "(you)" and blocks self-removal. */
    public fun ownLeafIndex(): UInt? = repo.ownLeafIndex(groupIdHex)

    /**
     * Human label for a roster member: a matched contact's chosen name (or its
     * fingerprint), else a short fingerprint of the signature key. "(you)" is
     * appended for the local leaf. Mirrors the ADR-0021 call-roster resolution,
     * keyed on the member's signature public key.
     */
    public fun memberLabel(member: GroupMember): String {
        val suffix = if (ownLeafIndex() == member.leafIndex) " (you)" else ""
        for (c in AcceptedContactsStore.load(repo.context)) {
            if (c.request.fromPubkey.contentEquals(member.signaturePublicKey)) {
                return c.displayLabel + suffix
            }
        }
        val fp = member.signaturePublicKey.take(4)
            .joinToString("") { "%02x".format(it.toInt() and 0xff) }
        return "Member $fp$suffix"
    }

    /**
     * Resolve the typed `@username` (G-4-verified) and add it to this group:
     * publish the add commit to the journal (own-marked) + ferry the Welcome.
     * Fail-closed — a resolve / verify / publish failure surfaces an error and
     * leaves the group untouched.
     */
    public fun onAddMember() {
        val username = mutableAddMemberInput.value.trim().removePrefix("@")
        if (username.isEmpty() || mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                val (accountId, signaturePubkey) = resolveRecipient(username)
                repo.addMemberToGroup(groupIdHex, accountId, signaturePubkey)
                mutableAddMemberInput.value = ""
                refreshMembers()
            } catch (e: Throwable) {
                mutableMembershipError.value = e.message ?: e.toString()
                android.util.Log.w("ChatViewModel", "onAddMember failed", e)
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * Remove a member by leaf index: publish the removal commit to the journal
     * (own-marked). Refuses to remove the local leaf. Fail-closed on error.
     */
    public fun onRemoveMember(leafIndex: UInt) {
        if (ownLeafIndex() == leafIndex) {
            mutableMembershipError.value = "You cannot remove yourself from the group."
            return
        }
        if (mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                repo.removeMemberFromGroup(groupIdHex, leafIndex)
                refreshMembers()
            } catch (e: Throwable) {
                mutableMembershipError.value = e.message ?: e.toString()
                android.util.Log.w("ChatViewModel", "onRemoveMember failed", e)
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * Promote / demote a member as admin (owner only — the toggle is shown only
     * when [isOwner]). Publishes the `setGroupRoles` commit. Fail-closed.
     */
    public fun onToggleAdmin(member: GroupMember, makeAdmin: Boolean) {
        if (mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                repo.setMemberAdmin(groupIdHex, member.signaturePublicKey, makeAdmin)
                refreshMembers()
            } catch (e: Throwable) {
                mutableMembershipError.value = e.message ?: e.toString()
                android.util.Log.w("ChatViewModel", "onToggleAdmin failed", e)
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * ADR-0027 (G-3) — approve a pending external-join request (owner/admin
     * only). Drives the role-gated approve (fail-closed in the core), broadcasts
     * the commit + posts the wrapped Welcome rendezvous, then refreshes the
     * roster + pending list. Fail-closed on error.
     */
    public fun onApproveJoin(proposalRef: ByteArray) {
        if (mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                repo.approveExternalJoin(groupIdHex, proposalRef)
                refreshMembers()
            } catch (e: Throwable) {
                mutableMembershipError.value = e.message ?: e.toString()
                android.util.Log.w("ChatViewModel", "onApproveJoin failed", e)
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * ADR-0027 (G-3) — reject a pending external-join request (owner/admin only).
     * Local-only drop of the retained proposal; no commit / journal post.
     * Fail-closed on error.
     */
    public fun onRejectJoin(proposalRef: ByteArray) {
        if (mutableMembershipBusy.value) return
        mutableMembershipBusy.value = true
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                repo.rejectExternalJoin(groupIdHex, proposalRef)
                refreshMembers()
            } catch (e: Throwable) {
                mutableMembershipError.value = e.message ?: e.toString()
                android.util.Log.w("ChatViewModel", "onRejectJoin failed", e)
            } finally {
                mutableMembershipBusy.value = false
            }
        }
    }

    /**
     * ADR-0028 (G-3) — mint a group invite-link (owner/admin only) and publish
     * it to [inviteLink] for the members sheet to show + copy. Fail-closed.
     */
    public fun onCreateInviteLink() {
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                mutableInviteLink.value = repo.createGroupInviteLink(groupIdHex)
            } catch (e: Throwable) {
                mutableMembershipError.value = e.message ?: e.toString()
                android.util.Log.w("ChatViewModel", "onCreateInviteLink failed", e)
            }
        }
    }

    /**
     * ADR-0028 revoke — revoke the currently-shown group invite link (issuer
     * only). A pointer link carries a ledger blob the server deletes; an inline
     * link has nothing to revoke (we just clear the field). Fail-closed.
     */
    public fun onRevokeInviteLink() {
        val link = mutableInviteLink.value ?: return
        mutableMembershipError.value = null
        viewModelScope.launch {
            try {
                repo.revokeGroupInviteLink(link)
                mutableInviteLink.value = null
            } catch (e: Throwable) {
                mutableMembershipError.value = e.message ?: e.toString()
                android.util.Log.w("ChatViewModel", "onRevokeInviteLink failed", e)
            }
        }
    }

    /**
     * Start a multi-party SFU group call for this (group) chat. Mints the
     * call-admission JWT, derives the per-call SFrame seed + leaf from the
     * MLS exporter, and connects through [SfuCallUi]. Fails closed (logs +
     * no call) on any missing prerequisite — SFU URL, session token, or
     * call-key derivation (security invariant #4 — never fall back to
     * non-E2EE).
     */
    public fun startGroupCall(video: Boolean = false) {
        if (mutableActiveSfuCall.value != null || mutableActiveCall.value != null) return
        val base = SfuConfig.baseUrl ?: run {
            android.util.Log.w(
                "ChatViewModel",
                "startGroupCall: SFU URL not configured (lakko.sfu.url) — refusing",
            )
            return
        }
        val token = sessionTokenProvider() ?: run {
            android.util.Log.w(
                "ChatViewModel",
                "startGroupCall: no session token — re-authenticate to start a group call",
            )
            return
        }
        val callIdBytes = CallSession.mintCallId()
        val callIdHex = callIdBytes.joinToString("") { "%02x".format(it.toInt() and 0xff) }
        // Mark connecting up front so the externally-driven CallScreen
        // never flashes the prejoin (Join) screen for an auto-connecting
        // SFU call — the overlay appears only once `activeSfuCall` flips
        // non-null, by which point the phase is already Connecting.
        sfuCallUiState.setPhase(CallUiState.CallPhase.Connecting)
        viewModelScope.launch {
            val key = repo.deriveCallKey(groupIdHex = groupIdHex, callId = callIdBytes)
            if (key == null) {
                android.util.Log.w(
                    "ChatViewModel",
                    "startGroupCall: call-key derivation failed — refusing per invariant #4",
                )
                sfuCallUiState.setPhase(CallUiState.CallPhase.Ended(System.currentTimeMillis()))
                return@launch
            }
            val jwt = CallJwtClient()
            try {
                jwt.setSessionToken(token)
                val minted = jwt.mint(callId = callIdHex)
                val config = SfuCallSession.Config(
                    sfuBaseUrl = base,
                    jwt = minted.jwt,
                    seed = key.seed,
                    localLeaf = key.localLeaf,
                    audio = true,
                    video = video,
                )
                // ADR-0021 — precompute the per-call peer_tag -> contact
                // label map once over the persisted accepted contacts, so
                // the SFU roster resolves to the identifier the rest of
                // the app shows. The tag is re-derived locally (the SFU
                // never sends a name); a collision falls back to the
                // pseudonym.
                val tagToLabel = HashMap<String, String>()
                val collidedTags = HashSet<String>()
                for (c in AcceptedContactsStore.load(repo.context)) {
                    val accountId = c.request.fromAccountId ?: continue
                    val accountIdHex =
                        accountId.joinToString("") { "%02x".format(it.toInt() and 0xff) }
                    val tag = sfuPeerTag(callId = callIdHex, accountId = accountIdHex)
                    if (tagToLabel.containsKey(tag)) {
                        collidedTags.add(tag)
                    } else {
                        // Prefer the user-chosen display name; fall back to
                        // the short pubkey fingerprint (ADR-0021 follow-up).
                        tagToLabel[tag] = c.displayLabel
                    }
                }
                val resolveDisplayName: (String, String, String) -> String = { _, pseudonym, peerTag ->
                    if (peerTag.isNotEmpty() && !collidedTags.contains(peerTag) &&
                        tagToLabel.containsKey(peerTag)
                    ) {
                        tagToLabel.getValue(peerTag)
                    } else if (pseudonym.isEmpty()) {
                        "Peer"
                    } else {
                        pseudonym.take(8)
                    }
                }
                val handle = SfuCallUi(
                    applicationContext = repo.context,
                    config = config,
                    scope = viewModelScope,
                    ui = sfuCallUiState,
                    resolveDisplayName = resolveDisplayName,
                )
                // G-18 / ADR-0029 — retain the call-id BEFORE the handle
                // goes live so the mid-call rekey collector (which guards on
                // `activeSfuCall != null`) can re-derive the seed at a new
                // epoch.
                activeSfuCallId = callIdBytes
                mutableActiveSfuCall.value = handle
                handle.connect()
            } catch (cause: Throwable) {
                android.util.Log.w("ChatViewModel", "startGroupCall failed", cause)
                mutableActiveSfuCall.value?.hangUp()
                mutableActiveSfuCall.value = null
                activeSfuCallId = null
                sfuCallUiState.setPhase(CallUiState.CallPhase.Ended(System.currentTimeMillis()))
            } finally {
                jwt.close()
            }
        }
    }

    /** Hang up the active SFU group call + clear it. */
    public fun hangUpGroupCall() {
        mutableActiveSfuCall.value?.hangUp()
        mutableActiveSfuCall.value = null
        activeSfuCallId = null
    }

    private fun makeSession(callId: ByteArray): CallSession =
        CallSession(
            applicationContext = repo.context,
            callId = callId,
        ) { bytes ->
            viewModelScope.launch {
                publishCallSignaling(bytes)
            }
        }

    private suspend fun publishCallSignaling(bytes: ByteArray) {
        try {
            repo.publishApplicationMessage(
                groupIdHex = groupIdHex,
                plaintextBytes = bytes,
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatViewModel", "publishCallSignaling failed", cause)
        }
    }

    private suspend fun handleIncomingCallSignaling(payloadBytes: ByteArray) {
        val parsed: FfiCallSignalingBody = try {
            parseCallSignalingBytes(payloadBytes)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatViewModel", "parseCallSignaling failed", cause)
            return
        }
        val bodyCallId = CallSession.callIdOf(parsed)
        val active = mutableActiveCall.value
        if (active == null) {
            if (parsed is FfiCallSignalingBody.Offer) {
                val session = makeSession(bodyCallId)
                mutableActiveCall.value = session
                // G3-2-call-key — arm the SFrame seed now, while
                // the call is ringing, so it is ready before the
                // user taps Accept (acceptCall arms SFrame).
                armCallKey(session, bodyCallId)
                session.applyIncomingSignaling(parsed)
            }
            return
        }
        if (!active.callId.contentEquals(bodyCallId)) return
        active.applyIncomingSignaling(parsed)
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

    /** Phase 2a3e2c-Android — read-cursor broadcast tracker.
     *  Highest broadcast messageId; the per-`messages` effect
     *  schedules a fresh `ReadCursor` payload only when the
     *  newest visible row's id exceeds this value. */
    private var lastBroadcastReadId: Long = -1L
    private var readCursorJob: Job? = null
    private val READ_CURSOR_DEBOUNCE_MS: Long = 1500L

    // -------------------------------------------------------------
    // ADR-0024 — typing indicator + read-receipt SEND state
    // -------------------------------------------------------------

    /** ADR-0024 — wall-clock time (millis) we last published a
     *  `typing:true`; the throttle suppresses a fresh `true` until
     *  [TYPING_SEND_THROTTLE_MS] has elapsed. */
    private var lastTypingTrueSentMs: Long = 0L
    /** `true` between the first throttled `typing:true` and the
     *  matching `typing:false`; lets us send `false` exactly once
     *  on blur / send (no `false` spam when nothing is in flight). */
    private var typingActive: Boolean = false
    private val TYPING_SEND_THROTTLE_MS: Long = 4_000L

    /** ADR-0024 — our own last-PUBLISHED read-receipt watermark.
     *  We re-publish `read_up_to` only when the highest received
     *  messageId advances past this (monotone + de-duped). */
    private var lastPublishedReadReceipt: Long = -1L
    private var readReceiptJob: Job? = null
    private val READ_RECEIPT_DEBOUNCE_MS: Long = 1_200L

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

        // Phase 2a3e2c-Android — read-cursor broadcast. Every
        // time the repo's per-thread message list grows past
        // the previously-broadcast value, schedule a fresh
        // `ReadCursor` payload pointing at the newest visible
        // message-id. Debounced 1500 ms.
        viewModelScope.launch {
            repo.state
                .distinctUntilChanged { a, b ->
                    val ar = a.threads.firstOrNull { it.groupIdHex == groupIdHex }
                    val br = b.threads.firstOrNull { it.groupIdHex == groupIdHex }
                    ar?.messages?.size == br?.messages?.size
                }
                .collect { state ->
                    val thread = state.threads.firstOrNull {
                        it.groupIdHex == groupIdHex
                    } ?: return@collect
                    scheduleReadCursorBroadcast(
                        messages = thread.messages,
                        groupIdBytes = gid,
                    )
                }
        }
    }

    private fun scheduleReadCursorBroadcast(
        messages: List<ChatMessage>,
        groupIdBytes: ByteArray,
    ) {
        if (messages.isEmpty()) return
        // Find the highest server-assigned messageId. Outgoing
        // rows with messageId == 0 (pre-ack) get skipped — the
        // ack path triggers a fresh effect.
        var highest: Long = -1L
        for (m in messages) {
            if (m.messageId > highest) highest = m.messageId
        }
        if (highest <= lastBroadcastReadId) return
        val target = highest
        readCursorJob?.cancel()
        readCursorJob = viewModelScope.launch {
            delay(READ_CURSOR_DEBOUNCE_MS)
            lastBroadcastReadId = target
            // 8 big-endian bytes — wire shape stable across
            // platforms.
            val idBytes = ByteArray(8)
            var v = target
            for (i in 7 downTo 0) {
                idBytes[i] = (v and 0xff).toByte()
                v = v ushr 8
            }
            val now = System.currentTimeMillis().toULong()
            try {
                deviceGroupSync?.setReadCursor(
                    groupId = groupIdBytes,
                    upToMessageId = idBytes,
                    readAtMs = now,
                )
            } catch (cause: Throwable) {
                android.util.Log.w(
                    "ChatViewModel",
                    "setReadCursor failed",
                    cause,
                )
            }
        }
    }

    /** ADR-0024 — the highest server-assigned messageId in this
     *  thread (the read-receipt watermark candidate). `-1` when
     *  there is no acked row yet (a pre-ack outgoing row has
     *  messageId == 0 and is skipped — the ack path re-triggers). */
    private fun highestMessageIdFor(repoState: me.lakko.app.chat.ChatRepositoryState): Long {
        val thread = repoState.threads.firstOrNull { it.groupIdHex == groupIdHex }
            ?: return -1L
        var highest = -1L
        for (m in thread.messages) {
            if (m.messageId > highest) highest = m.messageId
        }
        return highest
    }

    /**
     * ADR-0024 — publish a read receipt when [highest] advances
     * past our own last-published watermark. Monotone (a lower /
     * equal value is a no-op) + throttled (debounced
     * [READ_RECEIPT_DEBOUNCE_MS] so a burst of incoming rows
     * collapses to one publish). Tracks [lastPublishedReadReceipt]
     * so re-sync / repo re-emissions never re-publish.
     */
    private fun scheduleReadReceipt(highest: Long) {
        if (highest <= 0L) return
        if (highest <= lastPublishedReadReceipt) return
        val target = highest
        readReceiptJob?.cancel()
        readReceiptJob = viewModelScope.launch {
            delay(READ_RECEIPT_DEBOUNCE_MS)
            // Re-check under the post-delay state — a newer publish
            // may have landed while we waited.
            if (target <= lastPublishedReadReceipt) return@launch
            lastPublishedReadReceipt = target
            repo.sendReadReceipt(groupIdHex = groupIdHex, upTo = target)
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

    /**
     * ADR-0024 — render the typing indicator line from the set of
     * leaves currently typing, or `null` when nobody is.
     *
     *   * Our own leaf is never shown ("you typing" is noise).
     *   * 1×1 (a single other typist) → a bare "typing…" (the
     *     name is redundant in a two-person chat).
     *   * Exactly one named other in a group → "<name> is typing…"
     *     resolved via the same contact / fingerprint resolution
     *     the roster uses ([memberLabel], stripped of the "(you)"
     *     suffix it never carries for a non-self leaf).
     *   * More than one → "Several people are typing…".
     */
    private fun typingIndicatorText(typingLeaves: Set<UInt>): String? {
        if (typingLeaves.isEmpty()) return null
        val own = ownLeafIndex()
        val others = typingLeaves.filter { it != own }
        if (others.isEmpty()) return null
        // 1×1: the chat has exactly two members, so a single other
        // typist needs no name.
        val isOneToOne = (repo.memberCount(groupIdHex) ?: 0) <= 2
        if (others.size == 1) {
            if (isOneToOne) return "typing…"
            val name = labelForLeaf(others.first())
            return if (name != null) "$name is typing…" else "typing…"
        }
        return "Several people are typing…"
    }

    /** ADR-0024 — resolve a single MLS leaf to its member label
     *  (contact name or fingerprint), or `null` when the leaf is
     *  not in the live roster. Reuses [memberLabel]. */
    private fun labelForLeaf(leaf: UInt): String? {
        val member = repo.members(groupIdHex)?.firstOrNull { it.leafIndex == leaf }
            ?: return null
        return memberLabel(member)
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
            // ADR-0024 — typing indicator + read-state surface.
            typingText = typingIndicatorText(thread?.typingLeaves ?: emptySet()),
            readUpToByLeaf = thread?.readUpToByLeaf ?: emptyMap(),
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
        // ADR-0024 — a non-empty edit means the user is actively
        // typing: publish `typing:true` (throttled to at most once
        // per ~4 s). Clearing the field to empty is treated as a
        // stop (publish `typing:false` once).
        if (value.isNotEmpty()) {
            maybeSendTypingTrue()
        } else {
            sendTypingStop()
        }
    }

    /**
     * ADR-0024 — publish a throttled `typing:true`. At most one
     * `true` per [TYPING_SEND_THROTTLE_MS]; a live typist's
     * keystrokes inside that window collapse to a single signal.
     * Marks [typingActive] so the matching `false` fires exactly
     * once on blur / send.
     */
    private fun maybeSendTypingTrue() {
        val now = System.currentTimeMillis()
        if (typingActive && now - lastTypingTrueSentMs < TYPING_SEND_THROTTLE_MS) return
        lastTypingTrueSentMs = now
        typingActive = true
        viewModelScope.launch {
            repo.sendTyping(groupIdHex = groupIdHex, isTyping = true)
        }
    }

    /**
     * ADR-0024 — publish `typing:false` exactly once (best-effort
     * clear). A no-op when no `typing:true` is outstanding, so a
     * blur / send / field-clear can never spam `false`.
     */
    private fun sendTypingStop() {
        if (!typingActive) return
        typingActive = false
        viewModelScope.launch {
            repo.sendTyping(groupIdHex = groupIdHex, isTyping = false)
        }
    }

    /**
     * ADR-0024 — the composer's focus changed. On focus-loss we
     * clear any outstanding typing signal (`typing:false` once).
     * The screen wires this through `onFocusChanged`.
     */
    public fun onComposerFocusChanged(focused: Boolean) {
        if (!focused) sendTypingStop()
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

        // ADR-0024 — sending a message clears any outstanding
        // typing signal (`typing:false` once).
        sendTypingStop()

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
     * ADR-0023 — edit one of OUR OWN messages. Composes + publishes
     * the EDIT op + locally replaces the target's text (the DS
     * never echoes our own application messages back). A blank /
     * unchanged edit is a no-op. Failures surface through
     * [ChatScreenUiState.sendError].
     */
    public fun onEditMessage(targetMessageId: Long, newText: String) {
        val trimmed = newText.trim()
        if (trimmed.isEmpty()) return
        viewModelScope.launch {
            try {
                repo.editMessage(
                    groupIdHex = groupIdHex,
                    targetMessageId = targetMessageId,
                    newText = trimmed,
                )
            } catch (err: Throwable) {
                composer.update { it.copy(sendError = err.message ?: "Edit failed.") }
                android.util.Log.w("ChatViewModel", "onEditMessage failed", err)
            }
        }
    }

    /**
     * ADR-0023 — delete one of OUR OWN messages. Composes +
     * publishes the DELETE op + locally tombstones the target.
     */
    public fun onDeleteMessage(targetMessageId: Long) {
        viewModelScope.launch {
            try {
                repo.deleteMessage(
                    groupIdHex = groupIdHex,
                    targetMessageId = targetMessageId,
                )
            } catch (err: Throwable) {
                composer.update { it.copy(sendError = err.message ?: "Delete failed.") }
                android.util.Log.w("ChatViewModel", "onDeleteMessage failed", err)
            }
        }
    }

    /**
     * ADR-0023 — react to ANY message with [emoji]. Composes +
     * publishes the REACTION op + locally toggles our own
     * `(ownLeaf, emoji)` on the target. The FFI compose rejects an
     * empty / over-long emoji.
     */
    public fun onReactToMessage(targetMessageId: Long, emoji: String) {
        if (emoji.isEmpty()) return
        viewModelScope.launch {
            try {
                repo.reactToMessage(
                    groupIdHex = groupIdHex,
                    targetMessageId = targetMessageId,
                    emoji = emoji,
                )
            } catch (err: Throwable) {
                composer.update { it.copy(sendError = err.message ?: "Reaction failed.") }
                android.util.Log.w("ChatViewModel", "onReactToMessage failed", err)
            }
        }
    }

    /** ADR-0023 — our own leaf in this chat group, used by the UI
     *  to render which reaction chips we contributed (and can
     *  toggle off). `null` when there is no live handle. */
    public fun ownChatLeafIndex(): UInt? = repo.ownLeafIndex(groupIdHex)

    /** ADR-0024 — the live roster's leaves MINUS our own. The
     *  Bubble's read-state ("Read" only when EVERY other member
     *  has read it) is gated on every leaf in this set. Empty when
     *  there is no live handle / no other member. */
    public fun otherChatLeaves(): Set<UInt> {
        val own = repo.ownLeafIndex(groupIdHex)
        return repo.members(groupIdHex)
            ?.map { it.leafIndex }
            ?.filter { it != own }
            ?.toSet()
            ?: emptySet()
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
     * ADR-0040 (G-35) — run the durable pending-commit outbox sweep for this
     * thread ONCE on open, BEFORE the first poll. Re-publishes any commit that
     * crashed mid-window on a previous run (re-arming the own-echo filter so the
     * first poll drops the re-fetched commit instead of hitting
     * `CannotDecryptOwnMessage`) and discards a torn-write orphan. Best-effort —
     * a failure leaves the entries for the next open's sweep.
     */
    public suspend fun sweepPendingCommitsOnOpen() {
        try {
            repo.sweepPendingCommits(groupIdHex)
        } catch (_: Throwable) {
            // best-effort — the next open retries.
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
    /** ADR-0024 — the typing-indicator line ("<name> is typing…" /
     *  "typing…" / "Several people are typing…"), or `null` when
     *  nobody is typing. Rendered just above the input bar. */
    val typingText: String? = null,
    /** ADR-0024 — the monotone per-leaf read watermark. The Bubble
     *  renders "Read" on an OWN message once EVERY other member's
     *  watermark `>=` that message's id (else "Sent"). */
    val readUpToByLeaf: Map<UInt, Long> = emptyMap(),
)

/** ADR-0024 — read state of an OWN outgoing message, derived from
 *  the per-leaf read watermarks. [None] for a not-yet-acked row
 *  (messageId == 0). */
public enum class OwnReadState { None, Sent, Read }

/**
 * ADR-0024 — compute the read state of an OWN message.
 *
 *   * [OwnReadState.None] when the message has no real DS id yet
 *     (messageId == 0) — no "Sent"/"Read" tick is shown.
 *   * [OwnReadState.Read] when EVERY OTHER member's read watermark
 *     `>=` this message's id (the whole group has read it).
 *   * [OwnReadState.Sent] otherwise.
 *
 * [ownLeaf] (excluded from the all-others check) + [otherLeaves]
 * (the live roster minus self) are supplied by the screen so the
 * helper is pure + unit-test-friendly.
 */
public fun ownReadStateFor(
    messageId: Long,
    ownLeaf: UInt?,
    otherLeaves: Set<UInt>,
    readUpToByLeaf: Map<UInt, Long>,
): OwnReadState {
    if (messageId == 0L) return OwnReadState.None
    if (otherLeaves.isEmpty()) return OwnReadState.Sent
    val allRead = otherLeaves.all { leaf ->
        (readUpToByLeaf[leaf] ?: -1L) >= messageId
    }
    return if (allRead) OwnReadState.Read else OwnReadState.Sent
}
