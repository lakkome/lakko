// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import java.util.Collections
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.push.DevicePushId
import me.lakko.app.push.PushClient
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.contacts.KIND_WELCOME
import me.lakko.app.contacts.wrapEnvelope
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.ds.DsClient
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.GroupMember
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.loadGroup

/**
 * In-memory + on-disk state holder for the user's MLS chats.
 *
 * **Owns the canonical [Identity]** for the current session —
 * built once via [bootstrap] from the OPAQUE export key, kept
 * alive until [clear] / [forgetDevice]. Every chat operation
 * threads this same handle through `ChatClient` so MLS state
 * mutations accumulate on a single OpenMLS provider. Throwaway
 * `Identity.fromSeed` per op (the simpler shape that
 * `ContactsRepository` uses for its stateless sign / decrypt
 * surface) would lose every ratchet advance.
 *
 * **Holds a per-group `Group` handle map** keyed by hex group
 * id. The Group instances carry MLS ratchet state; dropping
 * them mid-session would force a re-bootstrap from the
 * snapshot. The map is mutated under a coroutine [Mutex] so a
 * `sendMessage` and a parallel `pollAllGroups` don't race
 * against one another's [ChatClient] calls (the underlying
 * OpenMLS storage isn't internally synchronised for concurrent
 * writes against the same group).
 *
 * **Snapshot persistence** runs after every state-mutating call
 * via [IdentitySnapshotStore]. Failures are logged-and-swallowed
 * — the in-memory state is still correct; the next mutation
 * will retry the persist naturally.
 *
 * ## Lifecycle
 *
 *   * [bootstrap] is called by `AuthRepository.signup` /
 *     `login` / `recover` once the session's [IdentitySecrets]
 *     are known. It builds the canonical Identity from the
 *     export key and restores the on-disk snapshot if one
 *     exists. Idempotent for the same account id; replaces
 *     the Identity if called for a different account.
 *   * [clear] is called by `AuthRepository.logout` — drops the
 *     in-memory state but keeps the on-disk snapshot envelope
 *     intact, so the next login on this device picks up
 *     where we left off.
 *   * [forgetDevice] is called by `AuthRepository.forgetDevice`
 *     — additionally wipes the on-disk snapshot envelope; the
 *     only way back into the conversation history is from
 *     other devices' epochs forward (via MLS commits) once
 *     they catch up to this device's absence.
 *
 * 1.15f4b adds the Compose ChatScreen + ChatViewModel on top of
 * this; 1.15f4c wires `ContactsRepository.acceptContactRequest`
 * to call [startGroupWith] so an Accept turns into a real chat.
 */
public class ChatRepository(
    private val applicationContext: Context,
    private val chatClient: ChatClient = ChatClient(),
    private val dsClient: DsClient = DsClient(),
    private val discovery: DiscoveryClient = DiscoveryClient(),
    private val snapshotStore: IdentitySnapshotStore = IdentitySnapshotStore,
    private val knownGroupsStore: KnownGroupsStore = KnownGroupsStore(applicationContext),
) : AutoCloseable {

    /**
     * G3-2 — expose the application context so downstream
     * `CallSession` instances can hand it to
     * `WebRtcPeerConnection` (libwebrtc's
     * `PeerConnectionFactory.initialize` requires an
     * Android Context for native-library loading + audio
     * device manager). Read-only — the constructor still
     * owns the lifecycle.
     */
    public val context: Context get() = applicationContext

    /**
     * The canonical [Identity] for the current session. `null`
     * until [bootstrap] populates it. Holding the reference in
     * an [AtomicReference] means snapshot writes don't need to
     * lock the whole repository — the [Identity] handle is
     * thread-safe at the UniFFI level (Rust-side `Arc`).
     */
    /** 1.20e — process-lifetime scope for fire-and-forget push
     *  fan-out / subscribe coroutines. `SupervisorJob` so one
     *  failed leg doesn't tear down siblings; `Dispatchers.IO`
     *  because both legs are network-heavy. */
    private val pushScope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val canonicalIdentity = AtomicReference<Identity?>(null)

    /**
     * Account id (base64) of the session that bootstrapped this
     * repository. Used to key the on-disk snapshot envelope.
     * Wrapped in `AtomicReference` so reads from any thread
     * pick up the value the bootstrap coroutine wrote.
     */
    private val currentAccountIdB64 = AtomicReference<String?>(null)

    /**
     * Map of hex group id → live [Group] handle. Mutated under
     * [groupsLock] for the same-group operations; reads can be
     * lock-free via the snapshot returned by `Collections.unmodifiableMap`.
     */
    private val groups: MutableMap<String, GroupRecord> =
        Collections.synchronizedMap(mutableMapOf())

    /**
     * Coroutine mutex for serialising MLS state mutations on a
     * single group. Coarse-grained: one mutex per repository,
     * not per group. Phase-1 chat volume doesn't justify the
     * per-group fine-grained map; if a future chat fan-out
     * ever pushes back on this, swap in a
     * `ConcurrentHashMap<String, Mutex>`.
     */
    private val groupsLock = Mutex()

    private val mutableState: MutableStateFlow<ChatRepositoryState> =
        MutableStateFlow(ChatRepositoryState())

    public val state: StateFlow<ChatRepositoryState> = mutableState.asStateFlow()

    /**
     * Build the canonical Identity for the session from the
     * OPAQUE export key + restore any on-disk MLS snapshot.
     *
     * Idempotent for the same account id (early-returns).
     * Switching accounts (different `secrets.accountId`) wipes
     * the in-memory state first.
     *
     * Failures from snapshot restore are logged-and-swallowed:
     * a corrupted / stale envelope shouldn't block the user
     * from continuing — they just start with an empty
     * conversation list on this device, and peers re-bootstrap
     * via fresh KeyPackages.
     */
    public fun bootstrap(secrets: IdentitySecrets) {
        val newAccountIdB64 = base64UrlSafe(secrets.accountId)
        val existing = currentAccountIdB64.get()
        if (existing == newAccountIdB64) return
        if (existing != null) {
            // Different account — wipe in-memory state before
            // attaching the new Identity.
            wipeInMemoryState()
        }
        // ADR-0017: the canonical AccountIdentity is seeded from the
        // stable BIP39 phrase seed, NOT the password export key.
        val identity = Identity.fromSeed(secrets.accountIdentitySeed)
        // Restore the snapshot — best-effort. The Identity is
        // valid even if the restore fails (we just lose the
        // prior MLS state, peers re-bootstrap).
        val restored = runCatching {
            val blob = snapshotStore.getSnapshot(applicationContext, newAccountIdB64)
            if (blob != null) {
                identity.restoreStorage(blob)
                true
            } else {
                false
            }
        }.getOrDefault(false)

        canonicalIdentity.set(identity)
        currentAccountIdB64.set(newAccountIdB64)

        // Eagerly rebuild `Group` handles for every previously-
        // known group id (1.15f5). `restoreStorage` laid down
        // OpenMLS provider storage; without this loop the in-
        // memory map would still be empty + peers' commits to
        // those groups would miss `pollGroup`'s lookup until
        // the user re-opened each thread.
        //
        // Best-effort per group: a load failure on one row
        // (e.g. epoch advanced too far, OpenMLS state file
        // corrupted) doesn't block the others. The store
        // entry stays — future bootstrap retries the load + a
        // future sprint can wire a "drop stale entries"
        // sweeper. Silent failures match 1.15f4a's posture
        // (no logger plumbed yet; 1.15d7 wires a tracing sink).
        if (restored) {
            for (groupIdHex in knownGroupsStore.load(newAccountIdB64)) {
                try {
                    val groupId = hexToBytes(groupIdHex) ?: continue
                    val group = loadGroup(identity, groupId) ?: continue
                    groups[groupIdHex] = GroupRecord(group = group, groupId = groupId)
                    // 1.20e — subscribe to push wakes for each
                    // rehydrated group. Idempotent server-side
                    // (composite PK), free re-runs on cold start.
                    pushScope.launch { subscribeGroupPushBestEffort(groupId) }
                } catch (_: Throwable) {
                    // best-effort
                }
            }
            publishThreadList()
        }

        // Phase 2a3f1 — restore the plaintext message log on
        // top of the rehydrated Group handles. The two stores
        // are independent (MLS state ⊥ message timeline), so
        // a corrupt / missing log doesn't block the rest of
        // the bootstrap.
        val log = MessageLogStore.getLog(applicationContext, newAccountIdB64)
        if (log != null) {
            for ((groupIdHex, persisted) in log.groups) {
                val record = groups[groupIdHex] ?: continue
                val seeded = persisted.messages.mapNotNull { it.toChatMessage() }
                record.seed(messages = seeded, cursor = persisted.cursor)
            }
            publishThreadList()
        }

        mutableState.update { it.copy(bootstrapped = true) }
    }

    /** Decode a lowercase hex string into bytes. Returns `null`
     *  on any malformed input (odd length, non-hex chars) so
     *  the bootstrap loop can `continue` past a corrupted
     *  store entry instead of throwing. */
    private fun hexToBytes(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        val out = ByteArray(hex.length / 2)
        for (i in out.indices) {
            val hi = Character.digit(hex[i * 2], 16)
            val lo = Character.digit(hex[i * 2 + 1], 16)
            if (hi < 0 || lo < 0) return null
            out[i] = ((hi shl 4) or lo).toByte()
        }
        return out
    }

    /**
     * Start a 1×1 MLS group with [recipientAccountId].
     *
     * Steps:
     *   1. Fetch the recipient's KeyPackage from the discovery
     *      directory.
     *   2. `Group.create(identity)` — fresh group with us as the
     *      founding member.
     *   3. `group.addMemberFromWire(identity, keyPackageBytes)`
     *      — produces a `(commit, welcome)` pair. Per MLS the
     *      commit would be broadcast, but our group is
     *      one-of-one before this call, so we drop it.
     *   4. Wrap the welcome with [KIND_WELCOME] and post it to
     *      the recipient's stranger inbox via
     *      `DsClient.publishMailbox` keyed by
     *      `IdentityPseudonym.fromIdentity(recipientPubkey)`.
     *   5. Persist the MLS snapshot.
     *
     * Returns the new group's hex id so the caller can hand it
     * straight to the Chat UI route.
     *
     * @throws IllegalStateException when [bootstrap] hasn't
     *         been called yet.
     */
    public suspend fun startGroupWith(
        recipientAccountId: ByteArray,
        recipientSignaturePubkey: ByteArray,
    ): String {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )

        val keyPackageBytes = discovery.fetchKeyPackage(recipientAccountId)
            ?: throw ChatError(
                "recipient has no KeyPackage uploaded yet",
                IllegalStateException("fetchKeyPackage returned null"),
            )

        return groupsLock.withLock {
            val group = Group.create(identity)
            val addResult = try {
                group.addMemberFromWire(identity, keyPackageBytes)
            } catch (e: FfiException) {
                throw ChatError("addMemberFromWire failed", e)
            }
            // commit dropped — group is one-of-one before the
            // add, no broadcast needed.

            val welcomeEnvelope = wrapEnvelope(KIND_WELCOME, addResult.welcome)
            val recipientPseudonym =
                uniffi.lakko_ffi_uniffi.IdentityPseudonym
                    .fromIdentity(recipientSignaturePubkey)
                    .use { it.asBytes() }
            try {
                dsClient.publishMailbox(recipientPseudonym, welcomeEnvelope)
            } catch (e: Throwable) {
                throw ChatError("DS publishMailbox (Welcome) failed", e)
            }

            val groupId = group.groupId()
            val groupIdHex = DsClient.pseudonymHex(groupId)
            groups[groupIdHex] = GroupRecord(group = group, groupId = groupId)
            persistKnownGroupBestEffort(groupIdHex)
            persistSnapshotBestEffort()
            publishThreadList()
            // 1.20e — subscribe to push wakes for the just-
            // created group.
            pushScope.launch { subscribeGroupPushBestEffort(groupId) }
            groupIdHex
        }
    }

    /**
     * Join an MLS group from a `Welcome` blob (received via
     * `ContactsRepository.pollStrangerInbox` → forwarded by
     * `IncomingMailboxEvent.Welcome`).
     *
     * Returns the joined group's hex id.
     *
     * @throws IllegalStateException when [bootstrap] hasn't
     *         been called yet.
     */
    public suspend fun joinGroupFromWelcome(welcomeBytes: ByteArray): String {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        return groupsLock.withLock {
            val group = try {
                Group.joinFromWelcome(welcomeBytes, identity)
            } catch (e: FfiException) {
                throw ChatError("joinFromWelcome failed", e)
            }
            val groupId = group.groupId()
            val groupIdHex = DsClient.pseudonymHex(groupId)
            groups[groupIdHex] = GroupRecord(group = group, groupId = groupId)
            persistKnownGroupBestEffort(groupIdHex)
            persistSnapshotBestEffort()
            publishThreadList()
            // 1.20e — subscribe to push wakes for the just-
            // joined group.
            pushScope.launch { subscribeGroupPushBestEffort(groupId) }
            groupIdHex
        }
    }

    /**
     * Compose `(plaintext, attachments)` into the canonical
     * 1.19a JSON envelope, encrypt for the current epoch of
     * [groupIdHex], and post the wire bytes to the DS. Appends
     * the message to the in-memory timeline + persists the
     * snapshot.
     *
     * Phase 1.19c4: empty [attachments] ⇒ text-only envelope
     * (binary-identical to a `composeEnvelope(text = …,
     * attachments = [])` call, which the 1.19a parser also
     * accepts as a v=1 message). Empty [plaintext] is valid
     * when at least one attachment is present (attachment-only
     * message). Empty-and-empty is rejected at the call site
     * upstream — the view model and screen layer enforce that.
     */
    /**
     * Phase 3a10 — closure the UI attaches to dispatch
     * inbound call-signaling bytes into its
     * [me.lakko.app.calls.CallSession] state machine. `null`
     * when no call surface is attached; the per-record poll
     * just drops the payload.
     */
    @Volatile
    public var callSignalingSink: ((String, ByteArray) -> Unit)? = null

    /**
     * Phase 3a10 — publish raw plaintext bytes as an MLS
     * application message. Same MLS-wrap + DS-publish path
     * as [sendMessage] but skips the chat-envelope composer
     * and the timeline append — call signaling rides this
     * channel out-of-band.
     */
    /** G3-2-call-key — material for arming a call's SFrame
     *  encryptor: the per-call seed + this member's leaf index. */
    public class CallKeyMaterial(
        public val seed: ByteArray,
        public val localLeaf: Int,
    )

    /**
     * G3-2-call-key — derive the per-call SFrame seed + local
     * leaf index for a call inside [groupIdHex].
     *
     * The seed is the MLS exporter output
     * `export_secret(label = "lakko/call/v1", context = callId,
     * length = 32)` per audit-pack invariant C-1 — a
     * deterministic function of the group's current epoch
     * secret, so both the offerer and the answerer (same group,
     * same `callId`, same epoch) derive **identical** seed bytes;
     * the per-sender SFrame key then forks on each member's leaf
     * index. Returns `null` (the call must refuse per security
     * invariant #4 — never fall back to non-E2EE) when the
     * repository is not bootstrapped, the group handle is
     * missing, or the exporter call fails (e.g. epoch skew).
     */
    public suspend fun deriveCallKey(
        groupIdHex: String,
        callId: ByteArray,
    ): CallKeyMaterial? {
        val identity = canonicalIdentity.get() ?: run {
            android.util.Log.w("ChatRepository", "deriveCallKey: not bootstrapped")
            return null
        }
        return groupsLock.withLock {
            val record = groups[groupIdHex]
            if (record == null) {
                android.util.Log.w("ChatRepository", "deriveCallKey: no group $groupIdHex")
                null
            } else {
                try {
                    val seed = record.group.exportSecret(
                        identity,
                        "lakko/call/v1",
                        callId,
                        32u,
                    )
                    val leaf = record.group.ownLeafIndex().toInt()
                    CallKeyMaterial(seed, leaf)
                } catch (cause: Throwable) {
                    android.util.Log.w("ChatRepository", "deriveCallKey: exporter failed", cause)
                    null
                }
            }
        }
    }

    /**
     * SFU group-call routing — the current MLS member count for
     * [groupIdHex], or `null` when the repository is not bootstrapped
     * or has no handle for that group. The chat-call trigger routes a
     * chat with >2 members through the multi-party SFU path and a
     * 2-member chat through the 1x1 P2P [me.lakko.app.calls.CallSession].
     * Reads are lock-free under [groupsLock] semantics (a concurrent
     * commit only ever grows / shrinks the count by one, which is fine
     * for a >2 routing decision).
     */
    public fun memberCount(groupIdHex: String): Int? {
        val record = groups[groupIdHex] ?: return null
        return try {
            record.group.memberCount().toInt()
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "memberCount failed for $groupIdHex", cause)
            null
        }
    }

    /**
     * Live MLS roster for [groupIdHex] at the current epoch — `(leafIndex,
     * signaturePublicKey)` per member — or `null` when there is no handle.
     * The group-chat UI matches each key against the accepted contacts to
     * render a name, else a fingerprint. Lock-free read (same as
     * [memberCount]); the UI MUST refresh after every add / remove.
     */
    public fun members(groupIdHex: String): List<GroupMember>? {
        val record = groups[groupIdHex] ?: return null
        return try {
            record.group.members()
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "members failed for $groupIdHex", cause)
            null
        }
    }

    /**
     * Our own leaf index in [groupIdHex], or `null` when there is no handle.
     * The roster marks the local member "(you)" and refuses self-removal.
     */
    public fun ownLeafIndex(groupIdHex: String): UInt? {
        val record = groups[groupIdHex] ?: return null
        return try {
            record.group.ownLeafIndex()
        } catch (cause: Throwable) {
            null
        }
    }

    /**
     * Add an already-resolved recipient to an EXISTING group (mirror of
     * [startGroupWith] but for a live group). The caller resolves +
     * G-4-verifies `@username -> (accountId, signaturePubkey)` first (the
     * pin-store verify lives in [me.lakko.app.contacts.ContactsClient]).
     *
     * Unlike the 1x1 create, the resulting commit MUST be published to the
     * group journal so existing members advance their epoch, and marked own
     * so the poll loop suppresses the echo — re-applying our own commit would
     * fork the MLS epoch. Returns the commit's DS message id.
     */
    public suspend fun addMemberToGroup(
        groupIdHex: String,
        recipientAccountId: ByteArray,
        recipientSignaturePubkey: ByteArray,
    ): Long {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        val keyPackageBytes = discovery.fetchKeyPackage(recipientAccountId)
            ?: throw ChatError(
                "recipient has no KeyPackage uploaded yet",
                IllegalStateException("fetchKeyPackage returned null"),
            )
        return groupsLock.withLock {
            val record = groups[groupIdHex]
                ?: throw ChatError(
                    "no live Group handle for $groupIdHex",
                    IllegalStateException("missing group record"),
                )
            val addResult = try {
                record.group.addMemberFromWire(identity, keyPackageBytes)
            } catch (e: FfiException) {
                throw ChatError("addMemberFromWire failed", e)
            }
            val commitPublish = try {
                dsClient.publishGroup(record.groupId, addResult.commit)
            } catch (e: Throwable) {
                throw ChatError("DS publishGroup (add-member commit) failed", e)
            }
            chatClient.markOwnPublishedCommit(commitPublish.id)
            val welcomeEnvelope = wrapEnvelope(KIND_WELCOME, addResult.welcome)
            val recipientPseudonym =
                uniffi.lakko_ffi_uniffi.IdentityPseudonym
                    .fromIdentity(recipientSignaturePubkey)
                    .use { it.asBytes() }
            try {
                dsClient.publishMailbox(recipientPseudonym, welcomeEnvelope)
            } catch (e: Throwable) {
                throw ChatError("DS publishMailbox (Welcome) failed", e)
            }
            persistSnapshotBestEffort()
            publishThreadList()
            commitPublish.id
        }
    }

    /**
     * Remove a member from an EXISTING group by leaf index. Publishes the
     * removal commit to the group journal (own-marked for echo suppression).
     * Returns the commit's DS message id.
     */
    public suspend fun removeMemberFromGroup(
        groupIdHex: String,
        leafIndex: UInt,
    ): Long {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        return groupsLock.withLock {
            val record = groups[groupIdHex]
                ?: throw ChatError(
                    "no live Group handle for $groupIdHex",
                    IllegalStateException("missing group record"),
                )
            val commitBytes = try {
                record.group.removeMember(identity, leafIndex)
            } catch (e: FfiException) {
                throw ChatError("removeMember failed", e)
            }
            val commitPublish = try {
                dsClient.publishGroup(record.groupId, commitBytes)
            } catch (e: Throwable) {
                throw ChatError("DS publishGroup (remove-member commit) failed", e)
            }
            chatClient.markOwnPublishedCommit(commitPublish.id)
            persistSnapshotBestEffort()
            publishThreadList()
            commitPublish.id
        }
    }

    public suspend fun publishApplicationMessage(
        groupIdHex: String,
        plaintextBytes: ByteArray,
    ) {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        groupsLock.withLock {
            chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = plaintextBytes,
            )
            persistSnapshotBestEffort()
        }
    }

    public suspend fun sendMessage(
        groupIdHex: String,
        plaintext: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
    ) {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )

        groupsLock.withLock {
            // 1.19c4: build the JSON envelope before
            // encrypting. `composeEnvelopeBytes` routes through
            // the UniFFI `composeEnvelope` free function;
            // wrong-length byte fields are rejected there
            // (defence-in-depth: the UI already validates the
            // AttachmentRefs it builds via
            // `AttachmentBridge.encryptAndUpload`).
            val envelopeBytes = try {
                AttachmentBridge.composeEnvelopeBytes(
                    text = plaintext,
                    attachments = attachments,
                )
            } catch (e: Throwable) {
                throw ChatError("envelope compose failed", e)
            }
            val result = chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = envelopeBytes,
            )
            record.appendOutgoing(
                text = plaintext,
                attachments = attachments,
                messageId = result.messageId,
                createdAt = result.createdAt,
            )
            persistSnapshotBestEffort()
            publishThreadList()
            // 1.20e — fan a wake out to every device subscribed
            // to this group. Off the user-visible critical path
            // so the PoW solve doesn't double send latency.
            // Best-effort — recipients still get the message via
            // foreground polling if push-gw is down.
            pushScope.launch {
                notifyPushGroupBestEffort(
                    groupId = record.groupId,
                    messageId = result.messageId,
                )
            }
        }
    }

    /** 1.20e — fire `PushClient.notifyGroup` on the side scope.
     *  Logged-and-swallowed; never re-thrown past sendMessage. */
    private suspend fun notifyPushGroupBestEffort(
        groupId: ByteArray,
        messageId: Long,
    ) {
        try {
            PushClient().use { client ->
                client.notifyGroup(
                    groupPseudonym = groupId,
                    envelopePointer = messageId,
                )
            }
        } catch (cause: Throwable) {
            android.util.Log.w(
                "lakko-chat",
                "push notify-group failed",
                cause,
            )
        }
    }

    /** 1.20e — subscribe this device to push wakes for the
     *  given group. Fired after every join / create / rehydrate.
     *  Server-side idempotent so re-runs are cheap no-ops. */
    private suspend fun subscribeGroupPushBestEffort(groupId: ByteArray) {
        try {
            val pseudonym = DevicePushId.getOrCreate(applicationContext)
            PushClient().use { client ->
                client.subscribeGroup(
                    devicePseudonym = pseudonym,
                    groupPseudonym = groupId,
                )
            }
        } catch (cause: Throwable) {
            android.util.Log.w(
                "lakko-chat",
                "push group subscribe failed",
                cause,
            )
        }
    }

    /**
     * Pull new messages from one group's journal + apply them
     * to the in-memory timeline.
     *
     * @param waitSecs DS long-poll budget. `0` returns
     *   immediately whatever's there (manual refresh); non-zero
     *   holds the connection — 1.15e7's continuous-polling
     *   `LaunchedEffect` passes `30`.
     */
    public suspend fun pollGroup(groupIdHex: String, waitSecs: Int = 0) {
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return

        groupsLock.withLock {
            val result = chatClient.pollMessages(
                identity = identity,
                group = record.group,
                after = record.cursor,
                waitSecs = waitSecs,
            )
            record.applyPollResult(result) { payload ->
                callSignalingSink?.invoke(groupIdHex, payload)
            }
            if (result.events.isNotEmpty()) {
                persistSnapshotBestEffort()
            }
            publishThreadList()
        }
    }

    /** Pull new messages for every active group. Best-effort:
     *  per-group failures are logged-and-swallowed so one bad
     *  apple doesn't block the others.
     *
     *  @param waitSecs same semantics as [pollGroup]; passed
     *    through to each per-group call. With 30s long-poll +
     *    sequential per-group iteration the worst-case latency
     *    to see a new message in any group is `30 * N` seconds
     *    where N is the group count — which is fine for the
     *    1×1 chat surface (one group per peer), and the
     *    refactor to parallel polling lands when group counts
     *    actually push back. */
    public suspend fun pollAllGroups(waitSecs: Int = 0) {
        val identity = canonicalIdentity.get() ?: return
        // Snapshot the keys outside the lock — pollGroup
        // re-acquires it per group.
        val groupIds = synchronized(groups) { groups.keys.toList() }
        for (groupIdHex in groupIds) {
            try {
                pollGroup(groupIdHex, waitSecs = waitSecs)
            } catch (_: Throwable) {
                // Best-effort: 1.15d7 will wire a tracing-
                // equivalent sink for these.
            }
        }
    }

    /**
     * Clear the in-memory state for the current session. Keeps
     * the on-disk MLS snapshot intact — `forgetDevice` is the
     * full-wipe path. Called from `AuthRepository.logout`.
     */
    public fun clear() {
        wipeInMemoryState()
        currentAccountIdB64.set(null)
        canonicalIdentity.set(null)
        mutableState.value = ChatRepositoryState()
    }

    /**
     * Full wipe: clear in-memory state + delete the on-disk
     * snapshot envelope. Called from
     * `AuthRepository.forgetDevice`.
     */
    public fun forgetDevice() {
        val accountIdB64 = currentAccountIdB64.get()
        clear()
        if (accountIdB64 != null) {
            try {
                snapshotStore.deleteSnapshot(applicationContext, accountIdB64)
            } catch (_: Throwable) {
                // Best-effort wipe; the user has already
                // forgotten the device, missing snapshot
                // delete is non-fatal.
            }
            try {
                knownGroupsStore.clear(accountIdB64)
            } catch (_: Throwable) {
                // Best-effort wipe — same posture as
                // snapshotStore.deleteSnapshot above.
            }
            // Phase 2a3f1 — drop the plaintext message log
            // too. The Keystore wrap key is shared across
            // accounts so we leave it intact; only the
            // per-account envelope file gets removed.
            try {
                MessageLogStore.deleteLog(applicationContext, accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }

    override fun close() {
        chatClient.close()
        dsClient.close()
        discovery.close()
        canonicalIdentity.getAndSet(null)?.close()
        synchronized(groups) {
            for (rec in groups.values) rec.group.close()
            groups.clear()
        }
    }

    // -----------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------

    private fun wipeInMemoryState() {
        synchronized(groups) {
            for (rec in groups.values) rec.group.close()
            groups.clear()
        }
        // Close the previous identity handle (releases the
        // Rust-side Arc + provider storage); the next
        // bootstrap builds a fresh one.
        canonicalIdentity.getAndSet(null)?.close()
    }

    /** Add [groupIdHex] to the per-account known-groups set so
     *  the next [bootstrap] can eagerly rebuild the handle.
     *  Best-effort: failures swallowed; a missing entry just
     *  forces the lazy `Group.create` / `joinFromWelcome` path
     *  on next session, which is fine. */
    private fun persistKnownGroupBestEffort(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            knownGroupsStore.add(accountIdB64, groupIdHex)
        } catch (_: Throwable) {
            // best-effort
        }
    }

    /** Save the canonical Identity's MLS snapshot, swallowing
     *  any failure. The in-memory state stays good — the next
     *  mutation will retry the persist naturally. */
    private fun persistSnapshotBestEffort() {
        val identity = canonicalIdentity.get() ?: return
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            val blob = identity.snapshotStorage()
            snapshotStore.putSnapshot(applicationContext, accountIdB64, blob)
        } catch (_: Throwable) {
            // Best-effort. 1.15d7 will plumb a `tracing`-
            // equivalent logger so failures here surface to
            // diagnostics.
        }
        // Phase 2a3f1 — co-located with the MLS snapshot
        // persist so every mutation that ratchets the group
        // also writes the resulting plaintext-message log.
        persistMessageLogBestEffort()
    }

    /** Capture `(cursor, messages)` per known group + write
     *  it to the encrypted message log (Phase 2a3f1).
     *  Best-effort — Keystore / FS failures swallowed; the
     *  in-memory state stays good and the next mutation
     *  retries naturally. */
    private fun persistMessageLogBestEffort() {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val slots = mutableMapOf<String, PersistedGroup>()
        for ((hex, record) in groups) {
            slots[hex] = PersistedGroup(
                cursor = record.cursor,
                messages = record.snapshot().map {
                    PersistedMessage.Companion.from(it)
                },
            )
        }
        MessageLogStore.putLog(
            context = applicationContext,
            accountIdB64 = accountIdB64,
            payload = MessageLogPayload(groups = slots),
        )
    }

    /** Snapshot the per-group state into the public StateFlow.
     *  Called after every mutation. */
    private fun publishThreadList() {
        val threads = synchronized(groups) {
            groups.values.map { it.toThread() }
        }
        mutableState.update { it.copy(threads = threads) }
    }

    /** URL-safe-ish base64 of the 16-byte account id, used as
     *  the filename for [IdentitySnapshotStore]. */
    private fun base64UrlSafe(accountId: ByteArray): String =
        me.lakko.app.auth.base64Encode(accountId)
}

// =============================================================================
// State shape
// =============================================================================

/** Snapshot of every ChatRepository field a UI cares about. */
public data class ChatRepositoryState(
    val threads: List<ChatThread> = emptyList(),
    val bootstrapped: Boolean = false,
)

/** One conversation thread — keyed by hex group id. */
public data class ChatThread(
    /** Hex-encoded MLS group id; used as the DS journal key
     *  and as the route argument for the 1×1 ChatScreen. */
    val groupIdHex: String,
    val messages: List<ChatMessage>,
    /** Cursor for the next [ChatClient.pollMessages] call. */
    val cursor: Long,
)

/** One message in a [ChatThread].
 *
 *  Phase 1.19c4: now carries an optional [attachments] list
 *  alongside [text]. Outgoing messages populate it on
 *  [ChatRepository.sendMessage]; incoming messages populate it
 *  from the parsed envelope on [ChatClient.pollMessages]. An
 *  empty list = pre-1.19-style text-only message; identical
 *  rendering to before. */
public data class ChatMessage(
    /** Server-assigned DS id (or 0 for an outgoing message not
     *  yet acknowledged by the DS — `sendMessage` populates
     *  this before the call returns, so in practice we always
     *  carry the real id). */
    val messageId: Long,
    /** Direction marker. `Outgoing` for messages we sent;
     *  `Incoming` for everything from a peer; `System` for
     *  MLS commit / proposal frames. */
    val direction: Direction,
    /** UTF-8 plaintext (empty for system rows). */
    val text: String,
    /** Zero or more attachments parsed from / staged for the
     *  1.19a JSON envelope. Empty list ⇒ pre-1.19 text-only
     *  message; UI renders it identically to before. */
    val attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
    /** ISO-8601 timestamp from the DS. */
    val createdAt: String,
)

public enum class Direction { Outgoing, Incoming, System }

/** Internal per-group record holding the live MLS handle +
 *  the timeline mutations the StateFlow surfaces.
 *
 *  Not a `data class` because the `Group` handle is a mutable
 *  UniFFI Object and shouldn't participate in `equals` /
 *  `hashCode`. */
private class GroupRecord(
    val group: Group,
    val groupId: ByteArray,
) {
    private val messages: MutableList<ChatMessage> =
        Collections.synchronizedList(mutableListOf())

    @Volatile
    var cursor: Long = 0
        private set

    fun appendOutgoing(
        text: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
        messageId: Long,
        createdAt: String,
    ) {
        messages.add(
            ChatMessage(
                messageId = messageId,
                direction = Direction.Outgoing,
                text = text,
                attachments = attachments,
                createdAt = createdAt,
            ),
        )
    }

    fun applyPollResult(
        result: PollMessagesResult,
        callSignalingSink: ((ByteArray) -> Unit)? = null,
    ) {
        for (event in result.events) {
            when (event) {
                is IncomingChatEvent.Application -> messages.add(
                    ChatMessage(
                        messageId = event.messageId,
                        direction = Direction.Incoming,
                        text = event.text,
                        attachments = event.attachments,
                        createdAt = event.createdAt,
                    ),
                )
                is IncomingChatEvent.System -> messages.add(
                    ChatMessage(
                        messageId = event.messageId,
                        direction = Direction.System,
                        text = systemRowText(event.systemKind),
                        createdAt = event.createdAt,
                    ),
                )
                is IncomingChatEvent.CallSignaling -> {
                    // Phase 3a10 — route into the
                    // CallSession state machine. Never
                    // touches the chat timeline.
                    callSignalingSink?.invoke(event.payloadBytes)
                }
            }
        }
        cursor = result.cursor
    }

    fun toThread(): ChatThread = ChatThread(
        groupIdHex = DsClient.pseudonymHex(groupId),
        messages = messages.toList(),
        cursor = cursor,
    )

    /** Read-only snapshot for the persistence layer. */
    fun snapshot(): List<ChatMessage> = messages.toList()

    /** Phase 2a3f1 — seed the in-memory timeline + cursor
     *  from a persistent snapshot. Called from
     *  [ChatRepository.bootstrap] after the live [Group]
     *  handle has been rehydrated. */
    fun seed(messages: List<ChatMessage>, cursor: Long) {
        synchronized(this.messages) {
            this.messages.clear()
            this.messages.addAll(messages)
        }
        this.cursor = cursor
    }

    private fun systemRowText(kind: SystemKind): String = when (kind) {
        SystemKind.Commit -> "[mls commit]"
        SystemKind.Proposal -> "[mls proposal]"
        SystemKind.ExternalJoinProposal -> "[mls external join]"
    }
}
