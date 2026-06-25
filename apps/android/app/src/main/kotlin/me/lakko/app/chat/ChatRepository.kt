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
import uniffi.lakko_ffi_uniffi.Role
import uniffi.lakko_ffi_uniffi.RoleTable
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
     * Our own ADR-0022 role in [groupIdHex] (OWNER / ADMIN / MEMBER), or `null`
     * for an open / legacy group with no role table (or no handle). The members
     * UI gates the add / remove affordances on this.
     */
    public fun ownRole(groupIdHex: String): Role? {
        val identity = canonicalIdentity.get() ?: return null
        val record = groups[groupIdHex] ?: return null
        return try {
            record.group.ownRole(identity)
        } catch (cause: Throwable) {
            null
        }
    }

    /**
     * The group's ADR-0022 role table (`owner` + `admins` signature keys), or
     * `null` for an open / legacy group. The members UI uses it to label the
     * per-member promote / demote toggle.
     */
    public fun groupRoles(groupIdHex: String): RoleTable? {
        val record = groups[groupIdHex] ?: return null
        return try {
            record.group.groupRoles()
        } catch (cause: Throwable) {
            null
        }
    }

    /**
     * Promote / demote a member as admin (owner only — enforced on the
     * receiving side). Reads the role table, adds or removes the member's
     * signature key from the admin set, and publishes the `setGroupRoles`
     * commit (own-marked for echo suppression). Returns the commit's DS id.
     */
    public suspend fun setMemberAdmin(
        groupIdHex: String,
        memberSignaturePubkey: ByteArray,
        makeAdmin: Boolean,
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
            val table = try {
                record.group.groupRoles()
            } catch (e: Throwable) {
                null
            } ?: throw ChatError(
                "group has no role table",
                IllegalStateException("no role table"),
            )
            val admins = table.admins
                .filterNot { it.contentEquals(memberSignaturePubkey) }
                .toMutableList()
            if (makeAdmin) {
                admins.add(memberSignaturePubkey)
            }
            val commitBytes = try {
                record.group.setGroupRoles(identity, table.owner, admins)
            } catch (e: FfiException) {
                throw ChatError("setGroupRoles failed", e)
            }
            val commitPublish = try {
                dsClient.publishGroup(record.groupId, commitBytes)
            } catch (e: Throwable) {
                throw ChatError("DS publishGroup (role change) failed", e)
            }
            chatClient.markOwnPublishedCommit(commitPublish.id)
            persistSnapshotBestEffort()
            publishThreadList()
            commitPublish.id
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

    /**
     * ADR-0027 (G-3) — the joiner signature keys of the external-join requests
     * this group has retained pending an owner/admin decision. Each entry is the
     * `proposalRef` handle [approveExternalJoin] / [rejectExternalJoin] take.
     * Empty when there are none (or no live Group handle for the id).
     */
    public fun pendingExternalJoins(groupIdHex: String): List<ByteArray> {
        val record = groups[groupIdHex] ?: return emptyList()
        return try {
            record.group.pendingExternalJoins()
        } catch (e: FfiException) {
            emptyList()
        }
    }

    /**
     * ADR-0027 (G-3) — approve a pending external-join request (owner/admin only;
     * the core role-gates it, fail-closed). Drives the MLS approve (reuses the
     * audited add path -> Commit + Welcome), broadcasts the commit to the group
     * journal (own-marked for echo suppression), then wraps the Welcome in a
     * WelcomeRendezvous keyed on the joiner signature key and posts it to the
     * SAME journal — the not-yet-member joiner polls the journal, matches the
     * envelope to its own key, and opens the inner Welcome. Existing members skip
     * the rendezvous entry (it is not an MLS message, so their receive loop drops
     * it). Returns the commit's DS message id.
     */
    public suspend fun approveExternalJoin(
        groupIdHex: String,
        proposalRef: ByteArray,
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
            val addResult = try {
                record.group.approveExternalJoin(identity, proposalRef)
            } catch (e: FfiException) {
                throw ChatError("approveExternalJoin failed", e)
            }
            val commitPublish = try {
                dsClient.publishGroup(record.groupId, addResult.commit)
            } catch (e: Throwable) {
                throw ChatError("DS publishGroup (approve commit) failed", e)
            }
            chatClient.markOwnPublishedCommit(commitPublish.id)
            val rendezvous = try {
                uniffi.lakko_ffi_uniffi.wrapWelcomeRendezvous(proposalRef, addResult.welcome)
            } catch (e: FfiException) {
                throw ChatError("wrapWelcomeRendezvous failed", e)
            }
            try {
                dsClient.publishGroup(record.groupId, rendezvous)
            } catch (e: Throwable) {
                throw ChatError("DS publishGroup (Welcome rendezvous) failed", e)
            }
            persistSnapshotBestEffort()
            publishThreadList()
            commitPublish.id
        }
    }

    /**
     * ADR-0027 (G-3) — reject a pending external-join request (owner/admin only).
     * Local-only: drops the retained proposal so it leaves the pending list. No
     * commit, no journal post — the joiner never receives a Welcome.
     */
    public suspend fun rejectExternalJoin(
        groupIdHex: String,
        proposalRef: ByteArray,
    ) {
        groupsLock.withLock {
            val record = groups[groupIdHex]
                ?: throw ChatError(
                    "no live Group handle for $groupIdHex",
                    IllegalStateException("missing group record"),
                )
            try {
                record.group.rejectExternalJoin(proposalRef)
            } catch (e: FfiException) {
                throw ChatError("rejectExternalJoin failed", e)
            }
            persistSnapshotBestEffort()
        }
    }

    /**
     * ADR-0028 (G-3) — mint a group invite-link (owner/admin side). Signs a
     * GroupInviteCapability over this group's raw MLS group_id with the local
     * identity's Ed25519 key and returns the https://lakko.me/g/<slug> link the
     * owner shares out-of-band. A non-member opens it, reads the current epoch
     * from the journal, and posts an external-join proposal an admin approves
     * (ADR-0027). The link conveys only the group_id -- it authorizes nobody;
     * the admin gate is the security boundary. `0` = never expires.
     */
    public fun createGroupInviteLink(groupIdHex: String): String {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("missing group record"),
            )
        val wire = try {
            uniffi.lakko_ffi_uniffi.composeGroupInviteCapability(
                identity.ed25519Seed(),
                record.group.groupId(),
                0u,
            )
        } catch (e: FfiException) {
            throw ChatError("composeGroupInviteCapability failed", e)
        }
        return "https://lakko.me/g/" + me.lakko.app.auth.base64UrlEncodeNoPad(wire)
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
            // ADR-0023 — stamp our own leaf as the author so a
            // later edit/delete op of this row (ours or a peer's
            // attempt) is gated against the real author.
            val ownLeaf = try {
                record.group.ownLeafIndex()
            } catch (_: Throwable) {
                null
            }
            record.appendOutgoing(
                text = plaintext,
                attachments = attachments,
                messageId = result.messageId,
                createdAt = result.createdAt,
                authorLeaf = ownLeaf,
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

    /**
     * ADR-0023 — edit one of OUR OWN messages: compose an EDIT op
     * envelope referencing [targetMessageId], publish it on the
     * application channel (own-marked, so the poll loop drops the
     * echo), and locally replace the target's text + mark it
     * edited. The DS never echoes our own application messages
     * back, so the local apply is the only place our own edit
     * surfaces. The receive-side author-only gate enforces that
     * peers accept this edit only because it really comes from
     * the target's author.
     */
    public suspend fun editMessage(
        groupIdHex: String,
        targetMessageId: Long,
        newText: String,
    ) {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        groupsLock.withLock {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeEditEnvelope(
                target = targetMessageId,
                newText = newText,
            )
            chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = envelopeBytes,
            )
            val ownLeaf = try {
                record.group.ownLeafIndex()
            } catch (_: Throwable) {
                null
            }
            record.applyOwnOp(
                op = MessageOp.Edit,
                target = targetMessageId,
                text = newText,
                emoji = null,
                ownLeaf = ownLeaf,
            )
            persistSnapshotBestEffort()
            publishThreadList()
        }
    }

    /**
     * ADR-0023 — delete one of OUR OWN messages: compose a DELETE
     * op envelope referencing [targetMessageId], publish it
     * (own-marked), and locally tombstone the target. Same
     * receive-side author-only gate as [editMessage].
     */
    public suspend fun deleteMessage(
        groupIdHex: String,
        targetMessageId: Long,
    ) {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        groupsLock.withLock {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeDeleteEnvelope(
                target = targetMessageId,
            )
            chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = envelopeBytes,
            )
            val ownLeaf = try {
                record.group.ownLeafIndex()
            } catch (_: Throwable) {
                null
            }
            record.applyOwnOp(
                op = MessageOp.Delete,
                target = targetMessageId,
                text = "",
                emoji = null,
                ownLeaf = ownLeaf,
            )
            persistSnapshotBestEffort()
            publishThreadList()
        }
    }

    /**
     * ADR-0023 — react to ANY message (ours or a peer's) with
     * [emoji]: compose a REACTION op envelope referencing
     * [targetMessageId], publish it (own-marked), and locally
     * toggle our own `(ownLeaf, emoji)` on the target. No author
     * gate — reacting to others' messages is the point. The FFI
     * compose throws on an empty / over-long emoji; the caller
     * surfaces that.
     */
    public suspend fun reactToMessage(
        groupIdHex: String,
        targetMessageId: Long,
        emoji: String,
    ) {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )
        groupsLock.withLock {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeReactionEnvelope(
                target = targetMessageId,
                emoji = emoji,
            )
            chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = envelopeBytes,
            )
            val ownLeaf = try {
                record.group.ownLeafIndex()
            } catch (_: Throwable) {
                null
            }
            record.applyOwnOp(
                op = MessageOp.Reaction,
                target = targetMessageId,
                text = "",
                emoji = emoji,
                ownLeaf = ownLeaf,
            )
            persistSnapshotBestEffort()
            publishThreadList()
        }
    }

    /**
     * ADR-0024 — publish an ephemeral TYPING signal to the group.
     * Composes a `{ typing: <isTyping> }` envelope + sends it on
     * the application channel (own-marked; the DS never echoes our
     * own application messages back, so there is no local
     * apply — typing is a sender→peers broadcast only). Throttling
     * (a `true` at most once per ~4 s) + the `false`-once-on-blur
     * discipline live in the ViewModel; this is the raw seam.
     *
     * Best-effort: a failure is logged + swallowed (a dropped
     * typing tick is invisible — the receiver auto-expires the
     * indicator anyway). It NEVER throws past the caller, so a
     * typing publish can never block the chat.
     */
    public suspend fun sendTyping(groupIdHex: String, isTyping: Boolean) {
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        try {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeTypingEnvelope(
                isTyping = isTyping,
            )
            groupsLock.withLock {
                chatClient.sendMessage(
                    identity = identity,
                    group = record.group,
                    plaintext = envelopeBytes,
                )
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "sendTyping failed", cause)
        }
    }

    /**
     * ADR-0024 — publish a READ-RECEIPT watermark to the group:
     * "I have read up to [upTo]" (a DS messageId). Composes a
     * `{ read_up_to: <upTo> }` envelope + sends it on the
     * application channel (own-marked). Monotone + throttle live
     * in the ViewModel (publish only when the watermark advances
     * past our own last-published value); this is the raw seam.
     *
     * Best-effort: a failure is logged + swallowed (the next
     * advance re-publishes). Never throws past the caller.
     */
    public suspend fun sendReadReceipt(groupIdHex: String, upTo: Long) {
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        try {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeReadReceiptEnvelope(
                upTo = upTo,
            )
            groupsLock.withLock {
                chatClient.sendMessage(
                    identity = identity,
                    group = record.group,
                    plaintext = envelopeBytes,
                )
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "sendReadReceipt failed", cause)
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
    /** ADR-0024 — the MLS leaf indices currently shown as "is
     *  typing", already filtered against the expiry window. The
     *  UI resolves each leaf to a member label (1×1 → bare
     *  "typing…"). Empty when nobody is typing. */
    val typingLeaves: Set<UInt> = emptySet(),
    /** ADR-0024 — the monotone per-leaf read watermark. The author
     *  renders "Read" on its OWN message once every OTHER member's
     *  watermark `>=` that message's id. */
    val readUpToByLeaf: Map<UInt, Long> = emptyMap(),
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
    /** ADR-0023 — the original author's MLS leaf index. Set for
     *  every chat message (our own outgoing rows carry our own
     *  leaf). The author-only edit/delete gate compares an op's
     *  sender leaf against this. `null` ⇒ unknown author (a
     *  rehydrated pre-ADR-0023 row, or a system row) — no op can
     *  pass the gate against it. */
    val authorLeaf: UInt? = null,
    /** ADR-0023 — `true` once a peer-accepted EDIT op replaced
     *  [text]; the UI appends a subtle "(edited)" marker. */
    val edited: Boolean = false,
    /** ADR-0023 — `true` once a peer-accepted DELETE op
     *  tombstoned the message; the UI renders a muted "message
     *  deleted" placeholder and drops the text + attachments. */
    val deleted: Boolean = false,
    /** ADR-0023 — accumulated reactions, attributed per sender
     *  leaf. Rendered as emoji chips with counts; re-sending the
     *  same `(leaf, emoji)` toggles it off. */
    val reactions: List<Reaction> = emptyList(),
)

/** ADR-0023 — one reaction on a [ChatMessage], attributed to its
 *  sender's MLS leaf for dedup + per-user toggle. */
public data class Reaction(
    val reactorLeaf: UInt,
    val emoji: String,
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

    /** ADR-0024 — ephemeral per-leaf typing state: the local
     *  monotonic-clock time (`SystemClock.elapsedRealtime` millis)
     *  at which we last observed a FRESH `typing:true` from that
     *  leaf. A leaf auto-expires [TYPING_EXPIRY_MS] after this
     *  stamp; an explicit `typing:false` removes the entry
     *  immediately. NEVER persisted (rebuilt purely from live
     *  signals). */
    private val typingByLeaf: MutableMap<UInt, Long> =
        Collections.synchronizedMap(mutableMapOf())

    /** ADR-0024 — monotone per-leaf read watermark: the highest
     *  read-up-to messageId each member has asserted. A lower
     *  value never overwrites a higher one (re-sync replay is
     *  idempotent). Session state — not persisted. */
    private val readUpToByLeaf: MutableMap<UInt, Long> =
        Collections.synchronizedMap(mutableMapOf())

    @Volatile
    var cursor: Long = 0
        private set

    fun appendOutgoing(
        text: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
        messageId: Long,
        createdAt: String,
        /** ADR-0023 — our own leaf at send time, so a peer's
         *  later edit/delete op of this row passes the gate only
         *  if it really comes from us. */
        authorLeaf: UInt?,
    ) {
        messages.add(
            ChatMessage(
                messageId = messageId,
                direction = Direction.Outgoing,
                text = text,
                attachments = attachments,
                createdAt = createdAt,
                authorLeaf = authorLeaf,
            ),
        )
    }

    /**
     * ADR-0023 — locally apply an op WE just sent (the DS never
     * echoes our own application messages back). The
     * author-only gate is trivially satisfied for our own edit /
     * delete (we authored the target); a reaction toggles our
     * own `(ownLeaf, emoji)`. Mirrors [applyOp] but skips the
     * cross-author gate the receive path enforces.
     */
    fun applyOwnOp(
        op: MessageOp,
        target: Long,
        text: String,
        emoji: String?,
        ownLeaf: UInt?,
    ) {
        applyOpInternal(
            op = op,
            target = target,
            text = text,
            emoji = emoji,
            senderLeaf = ownLeaf,
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
                        authorLeaf = event.senderLeaf,
                    ),
                )
                is IncomingChatEvent.Op -> applyOpInternal(
                    op = event.op,
                    target = event.target,
                    text = event.text,
                    emoji = event.emoji,
                    senderLeaf = event.senderLeaf,
                )
                is IncomingChatEvent.Typing -> applyTyping(
                    senderLeaf = event.senderLeaf,
                    isTyping = event.isTyping,
                    createdAt = event.createdAt,
                )
                is IncomingChatEvent.ReadReceipt -> applyReadReceipt(
                    senderLeaf = event.senderLeaf,
                    upTo = event.upTo,
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
                is IncomingChatEvent.ExternalJoinRequest -> {
                    // ADR-0027 (G-3) — a non-member's external Add proposal,
                    // retained pending an owner/admin approval. Not a timeline
                    // row; the members sheet reads the retained set via
                    // ChatRepository.pendingExternalJoins(groupIdHex). No fold.
                    Unit
                }
            }
        }
        cursor = result.cursor
    }

    /**
     * ADR-0023 — fold an edit / delete / reaction op into the
     * target message identified by its DS [target] id.
     *
     *   * edit / delete — **author-only gate**: applied only if
     *     [senderLeaf] equals the target's stored [ChatMessage.authorLeaf]
     *     (both non-null). Otherwise DROPPED (fail-closed,
     *     invariant #4). An unknown target is also dropped.
     *   * reaction — accepted from any member, attributed to
     *     [senderLeaf]; TOGGLE: an existing `(leaf, emoji)` pair
     *     is removed, else added. A `null` sender leaf is
     *     dropped (no attribution possible).
     *
     * LWW on the single journal: ops arrive in DS order and the
     * last edit/delete wins by construction (later op overwrites
     * earlier state).
     */
    private fun applyOpInternal(
        op: MessageOp,
        target: Long,
        text: String,
        emoji: String?,
        senderLeaf: UInt?,
    ) {
        synchronized(messages) {
            val idx = messages.indexOfFirst { it.messageId == target }
            if (idx < 0) return // unknown target — drop
            val current = messages[idx]
            when (op) {
                MessageOp.Edit -> {
                    // Author-only gate — both leaves must be known
                    // and equal, else drop.
                    if (senderLeaf == null || current.authorLeaf == null ||
                        senderLeaf != current.authorLeaf
                    ) {
                        return
                    }
                    if (current.deleted) return // a deleted message stays deleted
                    messages[idx] = current.copy(text = text, edited = true)
                }
                MessageOp.Delete -> {
                    if (senderLeaf == null || current.authorLeaf == null ||
                        senderLeaf != current.authorLeaf
                    ) {
                        return
                    }
                    messages[idx] = current.copy(
                        text = "",
                        attachments = emptyList(),
                        deleted = true,
                    )
                }
                MessageOp.Reaction -> {
                    val e = emoji ?: return
                    if (senderLeaf == null) return
                    val existing = current.reactions.indexOfFirst {
                        it.reactorLeaf == senderLeaf && it.emoji == e
                    }
                    val next = current.reactions.toMutableList()
                    if (existing >= 0) {
                        next.removeAt(existing) // toggle off
                    } else {
                        next.add(Reaction(reactorLeaf = senderLeaf, emoji = e))
                    }
                    messages[idx] = current.copy(reactions = next.toList())
                }
            }
        }
    }

    /**
     * ADR-0024 — fold an incoming TYPING signal into the
     * ephemeral per-leaf state.
     *
     *   * `senderLeaf == null` ⇒ dropped (no attribution).
     *   * STALE-IGNORE: a `true` whose DS [createdAt] is older
     *     than [TYPING_EXPIRY_MS] is dropped — on a full journal
     *     re-download a client would otherwise replay old
     *     `typing:true` rows and resurrect a stale indicator.
     *   * `true` (fresh) ⇒ stamp the leaf with the current
     *     monotonic time so it auto-expires [TYPING_EXPIRY_MS]
     *     later.
     *   * `false` ⇒ clear the leaf immediately (explicit stop).
     *
     * The UI reads [activeTypingLeaves] which re-filters against
     * the expiry window on every read, so an entry that is never
     * refreshed simply ages out without a timer here.
     */
    fun applyTyping(senderLeaf: UInt?, isTyping: Boolean, createdAt: String) {
        val leaf = senderLeaf ?: return
        if (!isTyping) {
            typingByLeaf.remove(leaf)
            return
        }
        // Stale-`createdAt` re-sync guard: ignore a typing:true
        // whose DS row is already older than the expiry window.
        val ageMs = typingSignalAgeMs(createdAt)
        if (ageMs != null && ageMs > TYPING_EXPIRY_MS) return
        typingByLeaf[leaf] = android.os.SystemClock.elapsedRealtime()
    }

    /**
     * ADR-0024 — fold an incoming READ-RECEIPT into the monotone
     * per-leaf watermark. A `null` leaf is dropped (no
     * attribution); a value `<=` the current watermark is ignored
     * (monotone — re-sync replay is idempotent).
     */
    fun applyReadReceipt(senderLeaf: UInt?, upTo: Long) {
        val leaf = senderLeaf ?: return
        val current = readUpToByLeaf[leaf] ?: -1L
        if (upTo > current) readUpToByLeaf[leaf] = upTo
    }

    /**
     * ADR-0024 — the set of leaves currently shown as "typing",
     * re-filtered against the expiry window at read time. A leaf
     * whose last fresh `typing:true` is older than
     * [TYPING_EXPIRY_MS] is auto-expired (and pruned from the
     * backing map as a side effect).
     */
    fun activeTypingLeaves(): Set<UInt> {
        val now = android.os.SystemClock.elapsedRealtime()
        synchronized(typingByLeaf) {
            val expired = typingByLeaf.filterValues { now - it > TYPING_EXPIRY_MS }.keys
            for (leaf in expired) typingByLeaf.remove(leaf)
            return typingByLeaf.keys.toSet()
        }
    }

    /** ADR-0024 — read-only snapshot of the monotone per-leaf read
     *  watermark for the author-side read-state render. */
    fun readUpToSnapshot(): Map<UInt, Long> =
        synchronized(readUpToByLeaf) { readUpToByLeaf.toMap() }

    fun toThread(): ChatThread = ChatThread(
        groupIdHex = DsClient.pseudonymHex(groupId),
        messages = messages.toList(),
        cursor = cursor,
        typingLeaves = activeTypingLeaves(),
        readUpToByLeaf = readUpToSnapshot(),
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

    private companion object {
        /** ADR-0024 — a typing indicator auto-expires this long
         *  after the last fresh `typing:true`; the same window is
         *  the stale-`createdAt` re-sync replay guard. ~6 s per
         *  the ADR (the sender re-emits every ~4 s while typing,
         *  so a live typist always refreshes inside this). */
        const val TYPING_EXPIRY_MS: Long = 6_000L

        /**
         * ADR-0024 — age (in millis) of a typing signal whose DS
         * row carries the ISO-8601 [createdAt], measured against
         * the local wall clock. `null` when the timestamp can't be
         * parsed — the caller then treats the signal as fresh
         * (never drops a live signal over a parse quirk; the
         * expiry window still bounds it). A negative age (DS clock
         * ahead of ours) is clamped to 0 so it counts as fresh.
         */
        fun typingSignalAgeMs(createdAt: String): Long? {
            val rowMs = parseIsoMillis(createdAt) ?: return null
            val age = System.currentTimeMillis() - rowMs
            return if (age < 0L) 0L else age
        }

        /** Parse a DS ISO-8601 / RFC-3339 timestamp to epoch
         *  millis. Tries an `Instant` (`...Z`) first, then an
         *  `OffsetDateTime` (explicit offset); `null` on any
         *  failure. */
        fun parseIsoMillis(value: String): Long? {
            return try {
                java.time.Instant.parse(value).toEpochMilli()
            } catch (_: java.time.format.DateTimeParseException) {
                try {
                    java.time.OffsetDateTime.parse(value)
                        .toInstant()
                        .toEpochMilli()
                } catch (_: java.time.format.DateTimeParseException) {
                    null
                }
            }
        }
    }
}
