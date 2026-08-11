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
import me.lakko.app.push.GroupPushRouting
import me.lakko.app.push.NativePushRoutingPorts
import me.lakko.app.push.PushClient
import me.lakko.app.push.PushRouteStore
import me.lakko.app.push.PushRoutingPorts
import me.lakko.app.push.PushRoutingSecretStore
import me.lakko.app.auth.IdentitySecrets
import uniffi.lakko_ffi_uniffi.derivePushGid
import me.lakko.app.contacts.KIND_WELCOME
import me.lakko.app.contacts.wrapEnvelope
import me.lakko.app.discovery.ConsumeInviteResult
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
    /** ADR-0042 — plaintext `push_gid -> (account, group)` reverse index the
     *  FCM silent-push decrypt reads (Sprint 4). Populated on every secret
     *  persist; the sensitive secret lives encrypted in
     *  [PushRoutingSecretStore]. */
    private val pushRouteStore: PushRouteStore = PushRouteStore(applicationContext),
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
                    // ADR-0042 legacy-sweep — provision opaque push routing for
                    // each rehydrated group: OWNER + no secret -> mint + persist
                    // + distribute the envelope + subscribe under push_gid; a
                    // member holding a secret -> subscribe under push_gid;
                    // anyone else -> defer (never subscribe under group_id).
                    // Idempotent + race-safe (per-group in-flight lock).
                    ensurePushRoutingBestEffort(group, identity)
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
            // ADR-0042 — the founder is the OWNER: mint the push-routing
            // secret + distribute it + subscribe under the derived push_gid.
            ensurePushRoutingBestEffort(group, identity)
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
            // ADR-0042 — a joiner is not the owner: `ensure` defers the
            // subscription (never under group_id) until the owner's
            // distribution envelope arrives on the journal, unless this
            // device already holds the secret (e.g. a prior receive).
            ensurePushRoutingBestEffort(group, identity)
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
            // ADR-0040 (G-35) — durable pending-commit outbox: persist the entry
            // (commit only; a role change has no Welcome) THEN the N+1 snapshot,
            // BOTH fail-closed, BEFORE any publish. A throw aborts the op (no
            // publish); the restart sweep reconciles the crash window. See
            // [sweepPendingCommits].
            //
            // ADR-0040 F1 residual (lakko-security) — reset-guard the pre-persist
            // entry build: `requireAccountId` / `group.epoch()` can throw AFTER the
            // eager merge (N -> N+1) and BEFORE runCrashSafeCommit, which would else
            // strand the in-memory group at the un-published N+1. No outbox entry
            // exists yet -> reset the group DIRECTLY. See [withPrePersistReset].
            val entry = withPrePersistReset(
                reset = { resetGroupToDurableSnapshotOrThrow(groupIdHex) },
            ) {
                PendingCommitEntry(
                    id = newOutboxEntryId(),
                    accountId = requireAccountId(),
                    groupId = outboxB64(record.groupId),
                    targetEpoch = record.group.epoch().toString(),
                    commitB64 = outboxB64(commitBytes),
                )
            }
            runCrashSafeCommit(
                persistEntryOrThrow = { CommitOutboxStore.putEntry(applicationContext, entry) },
                persistSnapshotOrThrow = { persistSnapshotDurableOrThrow() },
                // ADR-0040 F1 (lakko-security) — on a fail-closed persist-abort,
                // reset the eager-merged in-memory group (N+1) back to the durable
                // snapshot (N) so a subsequent same-session commit builds on N, not
                // the unpublished N+1. Fires ONLY on the persist-abort, never on a
                // publish failure (that commit is durably recorded — the sweep
                // re-publishes it). See [onCommitPersistAbort].
                onPersistAbort = { onCommitPersistAbort(groupIdHex, entry.id) },
                publish = {
                    val commitPublish = try {
                        dsClient.publishGroup(record.groupId, commitBytes)
                    } catch (e: Throwable) {
                        throw ChatError("DS publishGroup (role change) failed", e)
                    }
                    chatClient.markOwnPublishedCommit(commitPublish.id)
                    ackCommitBestEffort(entry.id, commitPublish.id, null)
                    publishThreadList()
                    commitPublish.id
                },
            )
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
        val addedCommitId = groupsLock.withLock {
            val record = groups[groupIdHex]
                ?: throw ChatError(
                    "no live Group handle for $groupIdHex",
                    IllegalStateException("missing group record"),
                )
            // 1. create-commit — eager-merges epoch N -> N+1 in the in-memory provider.
            val addResult = try {
                record.group.addMemberFromWire(identity, keyPackageBytes)
            } catch (e: FfiException) {
                throw ChatError("addMemberFromWire failed", e)
            }
            // Compute the second artifact (Welcome) + its mailbox destination
            // UP-FRONT so the outbox entry holds the EXACT bytes the restart
            // sweep will re-publish (no drift). The Welcome ships to the new
            // member's stranger inbox, addressed by their pubkey pseudonym.
            //
            // 2. ADR-0040 (G-35) — durable pending-commit outbox: persist the
            //    entry (commit + Welcome, BOTH artifacts) THEN the N+1 snapshot,
            //    BOTH fail-closed, BEFORE any publish. A throw aborts the op (no
            //    publish); the restart sweep re-publishes both. See [sweepPendingCommits].
            //
            // ADR-0040 F1 residual (lakko-security) — reset-guard the WHOLE
            // pre-persist region (second-artifact compute + entry build): any throw
            // here (wrapEnvelope / IdentityPseudonym.fromIdentity / requireAccountId
            // / group.epoch()) runs AFTER the eager merge (N -> N+1) and BEFORE
            // runCrashSafeCommit, and would else strand the in-memory group at the
            // un-published N+1. No outbox entry exists yet -> reset the group
            // DIRECTLY. See [withPrePersistReset].
            val (welcomeEnvelope, recipientPseudonym, entry) = withPrePersistReset(
                reset = { resetGroupToDurableSnapshotOrThrow(groupIdHex) },
            ) {
                val welcomeEnvelope = wrapEnvelope(KIND_WELCOME, addResult.welcome)
                val recipientPseudonym =
                    uniffi.lakko_ffi_uniffi.IdentityPseudonym
                        .fromIdentity(recipientSignaturePubkey)
                        .use { it.asBytes() }
                val entry = PendingCommitEntry(
                    id = newOutboxEntryId(),
                    accountId = requireAccountId(),
                    groupId = outboxB64(record.groupId),
                    targetEpoch = record.group.epoch().toString(),
                    commitB64 = outboxB64(addResult.commit),
                    secondArtifactB64 = outboxB64(welcomeEnvelope),
                    secondArtifactKind = SecondArtifactKind.WELCOME,
                    secondArtifactMailboxB64 = outboxB64(recipientPseudonym),
                )
                Triple(welcomeEnvelope, recipientPseudonym, entry)
            }
            runCrashSafeCommit(
                persistEntryOrThrow = { CommitOutboxStore.putEntry(applicationContext, entry) },
                persistSnapshotOrThrow = { persistSnapshotDurableOrThrow() },
                // ADR-0040 F1 (lakko-security) — on a fail-closed persist-abort,
                // reset the eager-merged in-memory group (N+1) back to the durable
                // snapshot (N) so a subsequent same-session commit builds on N, not
                // the unpublished N+1. Fires ONLY on the persist-abort, never on a
                // publish failure (that commit is durably recorded — the sweep
                // re-publishes it). See [onCommitPersistAbort].
                onPersistAbort = { onCommitPersistAbort(groupIdHex, entry.id) },
                publish = {
                    // 3. publish artifact 1 (commit) then artifact 2 (Welcome).
                    val commitPublish = try {
                        dsClient.publishGroup(record.groupId, addResult.commit)
                    } catch (e: Throwable) {
                        throw ChatError("DS publishGroup (add-member commit) failed", e)
                    }
                    chatClient.markOwnPublishedCommit(commitPublish.id)
                    val welcomePublish = try {
                        dsClient.publishMailbox(recipientPseudonym, welcomeEnvelope)
                    } catch (e: Throwable) {
                        throw ChatError("DS publishMailbox (Welcome) failed", e)
                    }
                    // 4. both artifacts on the wire -> ack (best-effort).
                    ackCommitBestEffort(entry.id, commitPublish.id, welcomePublish.id)
                    publishThreadList()
                    commitPublish.id
                },
            )
        }
        // ADR-0042 — re-broadcast the CURRENT push-routing secret so the
        // newly-added member learns it on their next poll (no-op if this
        // device holds no secret; idempotent for existing members). Runs
        // AFTER the add commit so the new member is already in the group.
        // Fire-and-forget. A SUCCESS flags the secret distributed, so this
        // path also heals a mint whose own broadcast failed. A FAILURE flags
        // the row PENDING, so the launch / chat-open sweep re-broadcasts it
        // until one lands. Without that rewind a group whose mint DID land
        // was already flagged distributed, so nothing ever re-sent and the
        // newly-added member never learned the secret at all — they defer
        // forever (never a fallback to the leaky group_id). Only the OWNER's
        // sweep acts on the pending flag; an ADMIN may also reach this path
        // (ADR-0022 lets an admin add members) and its mark is inert.
        redistributePushRoutingBestEffort(groupIdHex)
        return addedCommitId
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
            // ADR-0040 (G-35) — durable pending-commit outbox: persist the entry
            // (commit only; a remove has no Welcome) THEN the N+1 snapshot, BOTH
            // fail-closed, BEFORE the publish. A throw aborts the op (no publish);
            // the restart sweep reconciles the crash window. See [sweepPendingCommits].
            //
            // ADR-0040 F1 residual (lakko-security) — reset-guard the pre-persist
            // entry build: `requireAccountId` / `group.epoch()` can throw AFTER the
            // eager merge (N -> N+1) and BEFORE runCrashSafeCommit, which would else
            // strand the in-memory group at the un-published N+1. No outbox entry
            // exists yet -> reset the group DIRECTLY. See [withPrePersistReset].
            val entry = withPrePersistReset(
                reset = { resetGroupToDurableSnapshotOrThrow(groupIdHex) },
            ) {
                PendingCommitEntry(
                    id = newOutboxEntryId(),
                    accountId = requireAccountId(),
                    groupId = outboxB64(record.groupId),
                    targetEpoch = record.group.epoch().toString(),
                    commitB64 = outboxB64(commitBytes),
                )
            }
            runCrashSafeCommit(
                persistEntryOrThrow = { CommitOutboxStore.putEntry(applicationContext, entry) },
                persistSnapshotOrThrow = { persistSnapshotDurableOrThrow() },
                // ADR-0040 F1 (lakko-security) — on a fail-closed persist-abort,
                // reset the eager-merged in-memory group (N+1) back to the durable
                // snapshot (N) so a subsequent same-session commit builds on N, not
                // the unpublished N+1. Fires ONLY on the persist-abort, never on a
                // publish failure (that commit is durably recorded — the sweep
                // re-publishes it). See [onCommitPersistAbort].
                onPersistAbort = { onCommitPersistAbort(groupIdHex, entry.id) },
                publish = {
                    val commitPublish = try {
                        dsClient.publishGroup(record.groupId, commitBytes)
                    } catch (e: Throwable) {
                        throw ChatError("DS publishGroup (remove-member commit) failed", e)
                    }
                    chatClient.markOwnPublishedCommit(commitPublish.id)
                    ackCommitBestEffort(entry.id, commitPublish.id, null)
                    publishThreadList()
                    commitPublish.id
                },
            )
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

    /** ADR-0028 OQ2 (G-3) — advisory attribution for a pending request: which
     *  member minted the `/g/` link it arrived on, and whether that issuer is in
     *  the current roster. */
    public data class JoinAttributionInfo(
        val joinerSignatureKey: ByteArray,
        val issuerFingerprint: String,
        val issuerInRoster: Boolean,
    )

    /**
     * ADR-0028 OQ2 (G-3) — read the join-attribution envelopes from this group's
     * journal so the admin UI can show which member's `/g/` link each pending
     * request arrived on. For every valid attribution targeting this group (the
     * embedded capability re-verified via `parseGroupJoinAttribution`), returns
     * the joiner signature key, the issuer fingerprint, and whether the issuer is
     * in the current roster. Advisory — the role-gated approval gate is the
     * boundary, so failures degrade to an empty list.
     */
    public suspend fun fetchJoinAttributions(groupIdHex: String): List<JoinAttributionInfo> {
        val record = groups[groupIdHex] ?: return emptyList()
        val groupId = try {
            record.group.groupId()
        } catch (e: Throwable) {
            return emptyList()
        }
        val rosterKeys = try {
            record.group.members().map { it.signaturePublicKey }
        } catch (e: Throwable) {
            emptyList()
        }
        val raw = try {
            dsClient.fetchGroup(groupId, after = 0L, waitSecs = 0)
        } catch (e: Throwable) {
            return emptyList()
        }
        val out = mutableListOf<JoinAttributionInfo>()
        for (m in raw.messages) {
            val attr = try {
                uniffi.lakko_ffi_uniffi.parseGroupJoinAttribution(m.payload)
            } catch (e: Throwable) {
                continue
            }
            if (!attr.groupId.contentEquals(groupId)) continue
            out.add(
                JoinAttributionInfo(
                    joinerSignatureKey = attr.joinerSignatureKey,
                    issuerFingerprint = attr.issuerFingerprint,
                    issuerInRoster = rosterKeys.any { it.contentEquals(attr.issuerSignaturePubkey) },
                ),
            )
        }
        return out
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
            // 1. create-commit — reuses the audited add path, eager-merges N -> N+1.
            val addResult = try {
                record.group.approveExternalJoin(identity, proposalRef)
            } catch (e: FfiException) {
                throw ChatError("approveExternalJoin failed", e)
            }
            // Compute the second artifact (the rendezvous) UP-FRONT so the outbox
            // entry can hold both artifacts (two-artifact coverage). The
            // rendezvous rides the SAME group journal as the commit.
            //
            // 2. ADR-0040 (G-35) — durable pending-commit outbox: persist the
            //    entry (commit + rendezvous, BOTH artifacts) THEN the N+1
            //    snapshot, BOTH fail-closed, BEFORE any publish. A throw aborts
            //    the op (no publish); the restart sweep re-publishes both. See
            //    [sweepPendingCommits].
            //
            // ADR-0040 F1 residual (lakko-security) — reset-guard the WHOLE
            // pre-persist region (wrapWelcomeRendezvous + entry build): any throw
            // here runs AFTER the eager merge (N -> N+1) and BEFORE
            // runCrashSafeCommit, and would else strand the in-memory group at the
            // un-published N+1. No outbox entry exists yet -> reset the group
            // DIRECTLY. See [withPrePersistReset].
            val (rendezvous, entry) = withPrePersistReset(
                reset = { resetGroupToDurableSnapshotOrThrow(groupIdHex) },
            ) {
                val rendezvous = try {
                    uniffi.lakko_ffi_uniffi.wrapWelcomeRendezvous(proposalRef, addResult.welcome)
                } catch (e: FfiException) {
                    throw ChatError("wrapWelcomeRendezvous failed", e)
                }
                val entry = PendingCommitEntry(
                    id = newOutboxEntryId(),
                    accountId = requireAccountId(),
                    groupId = outboxB64(record.groupId),
                    targetEpoch = record.group.epoch().toString(),
                    commitB64 = outboxB64(addResult.commit),
                    secondArtifactB64 = outboxB64(rendezvous),
                    secondArtifactKind = SecondArtifactKind.RENDEZVOUS,
                )
                Pair(rendezvous, entry)
            }
            runCrashSafeCommit(
                persistEntryOrThrow = { CommitOutboxStore.putEntry(applicationContext, entry) },
                persistSnapshotOrThrow = { persistSnapshotDurableOrThrow() },
                // ADR-0040 F1 (lakko-security) — on a fail-closed persist-abort,
                // reset the eager-merged in-memory group (N+1) back to the durable
                // snapshot (N) so a subsequent same-session commit builds on N, not
                // the unpublished N+1. Fires ONLY on the persist-abort, never on a
                // publish failure (that commit is durably recorded — the sweep
                // re-publishes it). See [onCommitPersistAbort].
                onPersistAbort = { onCommitPersistAbort(groupIdHex, entry.id) },
                publish = {
                    // 3. publish artifact 1 (commit) so every existing member
                    //    admits the joiner, then artifact 2 (the rendezvous).
                    val commitPublish = try {
                        dsClient.publishGroup(record.groupId, addResult.commit)
                    } catch (e: Throwable) {
                        throw ChatError("DS publishGroup (approve commit) failed", e)
                    }
                    chatClient.markOwnPublishedCommit(commitPublish.id)
                    val rendezvousPublish = try {
                        dsClient.publishGroup(record.groupId, rendezvous)
                    } catch (e: Throwable) {
                        throw ChatError("DS publishGroup (Welcome rendezvous) failed", e)
                    }
                    // 4. both artifacts on the wire -> ack (best-effort).
                    ackCommitBestEffort(entry.id, commitPublish.id, rendezvousPublish.id)
                    publishThreadList()
                    commitPublish.id
                },
            )
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
    public suspend fun createGroupInviteLink(groupIdHex: String): String {
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
        // Production default (ADR-0028 OQ1): upload to the discovery ledger --
        // the server-side anti-abuse surface (per-joiner consume dedup + soft
        // max_uses) -- and share the short pointer URL. On an upload failure (a
        // censored / offline network) fall back to the inline form so the link
        // still works without the round-trip. `max_uses` is a soft throttle,
        // not an access control: the admin approval gate stays the boundary.
        return try {
            val blobId = discovery.uploadGroupInvite(wire, 0xFFFFFFFFL)
            composePointerGroupInviteUrl(blobId)
        } catch (e: Throwable) {
            try {
                composeInlineGroupInviteUrl(wire)
            } catch (fallbackError: Throwable) {
                "https://lakko.me/g/" + me.lakko.app.auth.base64UrlEncodeNoPad(wire)
            }
        }
    }

    /**
     * ADR-0028 revoke — the issuer revokes a previously-created group invite
     * link. Only the server-backed pointer form (a 32-byte blob_id) is
     * revocable; an inline link has no ledger row. Returns `true` when a ledger
     * row was revoked, `false` for an inline / malformed link (nothing to do).
     */
    public suspend fun revokeGroupInviteLink(link: String): Boolean {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        val idx = link.lastIndexOf("/g/")
        if (idx < 0) return false
        val slug = link.substring(idx + 3)
        val parsed = try {
            parseGroupInviteSlug(slug)
        } catch (e: GroupInviteUrlException) {
            return false
        }
        return when (parsed) {
            is ParsedGroupInvite.Inline -> false
            is ParsedGroupInvite.Pointer -> {
                try {
                    discovery.revokeGroupInvite(parsed.blobId, identity.ed25519Seed())
                } catch (e: Throwable) {
                    throw ChatError("group invite revoke failed", e)
                }
                true
            }
        }
    }

    /**
     * ADR-0028 (G-3) — resolve a `/g/<slug>` slug to the capability wire bytes,
     * fetching from the discovery ledger when the slug is a 32-byte
     * content-addressed pointer (the production form) or passing the bytes
     * straight through when it is an inline capability (the censored-region
     * fallback). Returns the wire bytes + the `blobId` (non-null only for the
     * pointer form) so the caller can drive the consume gate before requesting
     * to join.
     */
    public suspend fun resolveGroupInviteSlug(slug: String): Pair<ByteArray, ByteArray?> {
        val parsed = try {
            parseGroupInviteSlug(slug)
        } catch (e: GroupInviteUrlException) {
            throw ChatError("group invite link is malformed", e)
        }
        return when (parsed) {
            is ParsedGroupInvite.Inline -> Pair(parsed.capabilityBytes, null)
            is ParsedGroupInvite.Pointer -> {
                val fetched = try {
                    discovery.fetchGroupInvite(parsed.blobId)
                } catch (e: Throwable) {
                    throw ChatError("group invite fetch failed", e)
                } ?: throw ChatError(
                    "group invite not found or expired",
                    IllegalStateException("404"),
                )
                Pair(fetched, parsed.blobId)
            }
        }
    }

    /**
     * ADR-0028 (G-3) — joiner step 1: request to join a group from a `/g/` link
     * capability (the wire bytes the slug base64url-decodes to). Decodes +
     * verifies the capability fail-closed (before any post), reads the group's
     * current epoch from the journal head's public MLS framing
     * (`epochOfMessage`, no group secret), crafts an external-join proposal
     * (ADR-0027), and posts it to the group journal. Returns the group id + the
     * joiner's signature key — the caller drives [pollForJoinWelcome] until an
     * admin approves and the Welcome arrives.
     */
    public suspend fun requestToJoinFromLink(
        capabilityWire: ByteArray,
        nowMs: ULong,
        blobId: ByteArray? = null,
    ): Pair<ByteArray, ByteArray> {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        val cap = try {
            uniffi.lakko_ffi_uniffi.GroupInviteCapability.decodeAndVerify(capabilityWire)
        } catch (e: FfiException) {
            throw ChatError("group invite link decode/verify failed", e)
        }
        val groupId: ByteArray
        val valid: Boolean
        try {
            valid = cap.isValidNow(nowMs)
            groupId = cap.groupId()
        } finally {
            cap.close()
        }
        if (!valid) {
            throw ChatError(
                "group invite link has expired",
                IllegalStateException("expired"),
            )
        }
        // ADR-0028 OQ1 -- when the link was a server-backed pointer, register one
        // consume against the ledger's soft `max_uses` ceiling before posting the
        // request (anti-abuse defence-in-depth; the admin approval gate remains
        // the real boundary). A repeat from this joiner is idempotent. Inline
        // links (no `blobId`) skip this -- there is no ledger row.
        val accountIdB64 = currentAccountIdB64.get()
        if (blobId != null && accountIdB64 != null) {
            val joinerAccountId = me.lakko.app.auth.base64Decode(accountIdB64)
            val outcome = try {
                discovery.consumeGroupInvite(
                    blobId = blobId,
                    capabilityBytes = capabilityWire,
                    joinerAccountId = joinerAccountId,
                )
            } catch (e: Throwable) {
                throw ChatError("group invite consume failed", e)
            }
            when (outcome) {
                is ConsumeInviteResult.NotFound ->
                    throw ChatError(
                        "group invite not found or expired",
                        IllegalStateException("notFound"),
                    )
                is ConsumeInviteResult.Exhausted ->
                    throw ChatError(
                        "group invite has reached its use limit",
                        IllegalStateException("exhausted"),
                    )
                is ConsumeInviteResult.Consumed -> Unit // proceed
                is ConsumeInviteResult.AlreadyConsumed -> Unit // proceed
            }
        }
        // Read the current epoch from the journal head (public framing; a
        // non-member can do this). Empty / unreachable journal => epoch 0.
        var epoch: ULong = 0u
        try {
            val raw = dsClient.fetchGroup(groupId, after = 0L, waitSecs = 0)
            for (m in raw.messages) {
                uniffi.lakko_ffi_uniffi.epochOfMessage(m.payload)?.let { epoch = it }
            }
        } catch (e: Throwable) {
            // keep epoch 0
        }
        val proposal = try {
            identity.createExternalJoinProposal(groupId, epoch)
        } catch (e: FfiException) {
            throw ChatError("createExternalJoinProposal failed", e)
        }
        try {
            dsClient.publishGroup(groupId, proposal)
        } catch (e: Throwable) {
            throw ChatError("DS publishGroup (external-join proposal) failed", e)
        }
        // ADR-0028 OQ2 — post a join-attribution envelope so the approving admin
        // can see which member's link this request arrived on. Best-effort +
        // advisory: a failure must not block the join (the admin approval gate is
        // the security boundary).
        try {
            val attribution = uniffi.lakko_ffi_uniffi.composeGroupJoinAttribution(
                identity.ed25519Seed(),
                capabilityWire,
            )
            dsClient.publishGroup(groupId, attribution)
        } catch (e: Throwable) {
            // advisory only — ignore
        }
        return Pair(groupId, identity.signaturePublicKey())
    }

    /**
     * ADR-0028 (G-3) — joiner step 2: poll the group journal for the rendezvous
     * Welcome an admin posted after approving our request, and join from it. Run
     * on a poll loop after [requestToJoinFromLink]. Returns the joined group's
     * hex id on the first rendezvous addressed to our signature key, or `null`
     * if no Welcome has arrived yet (keep polling).
     */
    public suspend fun pollForJoinWelcome(
        groupId: ByteArray,
        joinerSignatureKey: ByteArray,
    ): String? {
        val raw = try {
            dsClient.fetchGroup(groupId, after = 0L, waitSecs = 0)
        } catch (e: Throwable) {
            throw ChatError("DS fetchGroup (join poll) failed", e)
        }
        for (m in raw.messages) {
            val rendezvous = try {
                uniffi.lakko_ffi_uniffi.parseWelcomeRendezvous(m.payload)
            } catch (e: FfiException) {
                continue
            }
            if (rendezvous.joinerSignatureKey.contentEquals(joinerSignatureKey)) {
                return joinGroupFromWelcome(rendezvous.welcome)
            }
        }
        return null
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
            // SECURITY (nonce-reuse-on-crash) — persist the
            // advanced (N+1) ratchet DURABLY before the DS
            // publish; a persist failure aborts the send inside
            // `chatClient.sendMessage` so no gen-N ciphertext
            // leaves the device. This path appends no local
            // bubble, so there is no after-publish message-log
            // persist to run.
            chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = plaintextBytes,
                persistRatchet = { persistSnapshotDurableOrThrow() },
            )
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
            // SECURITY (nonce-reuse-on-crash) — the (N+1) ratchet
            // is persisted DURABLY, fail-closed, BEFORE the DS
            // publish via `persistRatchet`; a persist failure
            // throws out of `chatClient.sendMessage` (before any
            // publish), surfacing to the ViewModel as a failed
            // send. The AFTER-publish persist below is the
            // best-effort plaintext message log only (it needs
            // the DS messageId that only exists post-publish) —
            // the security-critical ratchet snapshot already
            // landed durably above.
            val result = chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = envelopeBytes,
                persistRatchet = { persistSnapshotDurableOrThrow() },
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
            persistMessageLogBestEffort()
            publishThreadList()
            // 1.20e — fan a wake out to every device subscribed
            // to this group. Off the user-visible critical path
            // so the PoW solve doesn't double send latency.
            // Best-effort — recipients still get the message via
            // foreground polling if push-gw is down.
            // ADR-0042 — the wake routes under the opaque `push_gid`, not
            // the leaky DS `group_id`; no secret held -> the wake is
            // SKIPPED (never under group_id).
            pushScope.launch {
                notifyPushGroupBestEffort(
                    groupIdHex = groupIdHex,
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
            // SECURITY (nonce-reuse-on-crash) — persist the (N+1)
            // ratchet durably, fail-closed, BEFORE publish.
            chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = envelopeBytes,
                persistRatchet = { persistSnapshotDurableOrThrow() },
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
            persistMessageLogBestEffort()
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
            // SECURITY (nonce-reuse-on-crash) — persist the (N+1)
            // ratchet durably, fail-closed, BEFORE publish.
            chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = envelopeBytes,
                persistRatchet = { persistSnapshotDurableOrThrow() },
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
            persistMessageLogBestEffort()
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
            // SECURITY (nonce-reuse-on-crash) — persist the (N+1)
            // ratchet durably, fail-closed, BEFORE publish.
            chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = envelopeBytes,
                persistRatchet = { persistSnapshotDurableOrThrow() },
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
            persistMessageLogBestEffort()
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
                // SECURITY (nonce-reuse-on-crash) — a typing tick
                // still advances the MLS sender ratchet, so
                // persist the (N+1) state durably, fail-closed,
                // BEFORE publish. A persist failure aborts the
                // publish (no gen-N ciphertext on the wire) and is
                // swallowed below — a dropped typing tick is
                // invisible (the receiver auto-expires anyway).
                chatClient.sendMessage(
                    identity = identity,
                    group = record.group,
                    plaintext = envelopeBytes,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
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
                // SECURITY (nonce-reuse-on-crash) — a read-receipt
                // still advances the MLS sender ratchet, so
                // persist the (N+1) state durably, fail-closed,
                // BEFORE publish. A persist failure aborts the
                // publish (no gen-N ciphertext on the wire) and is
                // swallowed below — the next watermark advance
                // re-publishes.
                chatClient.sendMessage(
                    identity = identity,
                    group = record.group,
                    plaintext = envelopeBytes,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
                )
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "sendReadReceipt failed", cause)
        }
    }

    /** 1.20e / ADR-0042 — fire `PushClient.notifyGroup` on the side scope,
     *  UNDER the opaque `push_gid` (not the DS `group_id`). When this device
     *  holds no secret for the group yet the wake is SKIPPED rather than
     *  leaking `group_id` to push-gw (invariant #5); recipients still receive
     *  the message on their next foreground poll. Logged-and-swallowed. */
    private suspend fun notifyPushGroupBestEffort(
        groupIdHex: String,
        messageId: Long,
    ) {
        try {
            val pushGid = pushGidForGroupOrNull(groupIdHex) ?: return
            PushClient().use { client ->
                client.notifyGroup(
                    groupPseudonym = pushGid,
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

    /** ADR-0042 — subscribe this device to push-gw under [pushGid]. */
    private suspend fun subscribePushGidBestEffort(pushGid: ByteArray) {
        val pseudonym = DevicePushId.getOrCreate(applicationContext)
        PushClient().use { client ->
            client.subscribeGroup(
                devicePseudonym = pseudonym,
                groupPseudonym = pushGid,
            )
        }
    }

    /**
     * ADR-0042 — the `push_gid` this device would route [groupIdHex] under,
     * or `null` when it holds no `push_routing_secret` yet (the caller then
     * DEFERS / SKIPS rather than falling back to `group_id`). Derived from
     * the encrypted-at-rest secret via the audited keyed-BLAKE3 FFI.
     */
    private fun pushGidForGroupOrNull(groupIdHex: String): ByteArray? {
        val accountIdB64 = currentAccountIdB64.get() ?: return null
        val secret = PushRoutingSecretStore.get(applicationContext, accountIdB64, groupIdHex)
            ?: return null
        return try {
            derivePushGid(secret)
        } catch (_: Throwable) {
            null
        }
    }

    /**
     * ADR-0042 — build the production [PushRoutingPorts] for [group]: the FFI
     * derivation / compose + the encrypted secret store + the plaintext
     * reverse index + the push-gw subscribe + the journal broadcast (a
     * persist-before-publish control application message under [groupsLock]).
     */
    private fun pushRoutingPorts(group: Group, identity: Identity): PushRoutingPorts =
        NativePushRoutingPorts(
            context = applicationContext,
            accountIdB64 = currentAccountIdB64.get() ?: "",
            routeStore = pushRouteStore,
            subscribeFn = { pushGid -> subscribePushGidBestEffort(pushGid) },
            broadcastFn = { envelope -> broadcastControlToGroup(identity, group, envelope) },
        )

    /**
     * ADR-0042 — post a control application message (the push-routing-secret
     * distribution envelope) to [group]'s journal under [groupsLock], reusing
     * the audited persist-before-publish, fail-closed send primitive
     * ([ChatClient.sendMessage]): the sender ratchet advance is durably
     * persisted before the ciphertext reaches the DS, and the DS id is marked
     * own so the broadcaster's own poll drops the echo. Fires no push wake and
     * appends no timeline row (the repository does neither here).
     */
    private suspend fun broadcastControlToGroup(
        identity: Identity,
        group: Group,
        envelope: ByteArray,
    ) {
        groupsLock.withLock {
            chatClient.sendMessage(
                identity = identity,
                group = group,
                plaintext = envelope,
                persistRatchet = { persistSnapshotDurableOrThrow() },
            )
        }
    }

    /**
     * ADR-0042 — the owner-mint + subscribe funnel for [group]. Resolves the
     * ADR-0022 owner role (fail-closed to non-owner) and runs
     * [GroupPushRouting.ensure] on the push scope (fire-and-forget,
     * best-effort). Called at chat create / join + the bootstrap legacy-sweep.
     */
    private fun ensurePushRoutingBestEffort(group: Group, identity: Identity) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val groupIdHex = try {
            DsClient.pseudonymHex(group.groupId())
        } catch (_: Throwable) {
            return
        }
        val isOwner = try {
            group.ownRole(identity) == Role.OWNER
        } catch (_: Throwable) {
            false
        }
        pushScope.launch {
            try {
                GroupPushRouting.ensure(
                    accountIdB64 = accountIdB64,
                    groupIdHex = groupIdHex,
                    isOwner = isOwner,
                    ports = pushRoutingPorts(group, identity),
                )
            } catch (_: Throwable) {
                // best-effort — push is a UX nicety
            }
        }
    }

    /**
     * ADR-0042 — read [group]'s ADR-0022 role table: the owner key AND the
     * admin set, both raw 32-byte Ed25519 signature public keys (the same key
     * shape `group.members()[].signaturePublicKey` reports, so the gate
     * compares like with like). Distinguishes "no role table" (a legacy / open
     * group — the gate stays open) from "could not read" (fail closed), which
     * [GroupPushRouting.decideAcceptReceivedSecret] treats differently. Must be
     * called on the [groupsLock] critical path (the `Group` handle is not safe
     * to touch off it).
     */
    private fun roleTableOwner(group: Group): GroupPushRouting.OwnerLookup =
        try {
            val roles = group.groupRoles()
            if (roles == null) {
                GroupPushRouting.OwnerLookup.NoRoleTable
            } else {
                GroupPushRouting.OwnerLookup.Known(roles.owner, roles.admins)
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "push routing: role-table read failed", cause)
            GroupPushRouting.OwnerLookup.Unavailable
        }

    /**
     * ADR-0042 — resolve an MLS [leafIndex] to that member's 32-byte Ed25519
     * signature public key via the LIVE roster, or `null` when it cannot be
     * resolved (an external sender with no leaf, a leaf absent from the current
     * epoch's roster, or an FFI throw). `null` never matches an owner key, so
     * an unresolved sender is refused by the gate. Must be called on the
     * [groupsLock] critical path.
     */
    private fun signatureKeyForLeaf(group: Group, leafIndex: UInt?): ByteArray? {
        if (leafIndex == null) return null
        return try {
            group.members().firstOrNull { it.leafIndex == leafIndex }?.signaturePublicKey
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "push routing: roster read failed", cause)
            null
        }
    }

    /**
     * ADR-0042 — persist a received distribution-envelope secret + subscribe
     * under the derived `push_gid`. Fire-and-forget on the push scope
     * (best-effort). Never mints.
     *
     * GATED (lakko-security MEDIUM-1): the authority check used to live only on
     * the SEND side ([ensurePushRoutingBestEffort] gates the mint on
     * `ownRole == OWNER`), so on RECEIVE any member could unilaterally rotate
     * the group's `push_gid` — every member would overwrite its secret and
     * re-subscribe, and since per-group unsubscribe is never called, each churn
     * stranded an orphan push-gw subscription that keeps receiving wakes. The
     * MLS-authenticated [senderLeaf] is now resolved against the live roster
     * and ranked against the ADR-0022 role table: the OWNER may always rotate,
     * an ADMIN may only SEED a device holding nothing (ADR-0022 lets an admin
     * add members, and `redistribute` fires after any add-member commit — so
     * without this a member added by a non-owner admin would never get push
     * routing), and a plain member's CHANGED secret is DROPPED (no persist, no
     * subscribe). A byte-identical secret is always accepted, which keeps the
     * post-add-member `redistribute` idempotent for existing members.
     *
     * The two `Group` reads happen SYNCHRONOUSLY here — this runs inside the
     * poll loop's [groupsLock] critical section (via `applyPollResult`), the
     * same discipline as the `group.groupId()` read below and as
     * [ensurePushRoutingBestEffort]'s `ownRole` read. Only the store read and
     * the network subscribe are deferred to [pushScope].
     */
    private fun receivePushRoutingSecretBestEffort(
        group: Group,
        identity: Identity,
        secret: ByteArray,
        senderLeaf: UInt?,
    ) {
        val groupIdHex = try {
            DsClient.pseudonymHex(group.groupId())
        } catch (_: Throwable) {
            return
        }
        val owner = roleTableOwner(group)
        val senderKey = signatureKeyForLeaf(group, senderLeaf)
        pushScope.launch {
            try {
                val verdict = GroupPushRouting.receiveGated(
                    groupIdHex = groupIdHex,
                    secret = secret,
                    senderSignatureKey = senderKey,
                    owner = owner,
                    ports = pushRoutingPorts(group, identity),
                )
                if (verdict == GroupPushRouting.ReceiveVerdict.REJECT) {
                    android.util.Log.w(
                        "ChatRepository",
                        "push routing: refused a changed secret from an unauthorised " +
                            "sender (only the owner may rotate; an admin may only seed) " +
                            "(group=$groupIdHex, leaf=$senderLeaf)",
                    )
                }
            } catch (_: Throwable) {
                // best-effort — push is a UX nicety; never escapes the poll loop.
            }
        }
    }

    /**
     * ADR-0042 — the chat-open provisioning hook (mirror of the web
     * `Chat.tsx` onMount `provisionPushRouting`): re-run the owner-mint +
     * subscribe funnel for [groupIdHex] each time the thread is opened.
     *
     * The retry it drives is real only because the secret's row carries a
     * durable `distributed` flag: an owner whose distribution broadcast
     * failed keeps the row flagged pending, and this call (like the launch
     * sweep) re-broadcasts the SAME envelope, recomposed from the stored
     * secret. Before that flag existed the mint guard was permanently
     * satisfied by the persisted secret, so nothing but an unrelated
     * add-member `redistribute` could ever repair a failed broadcast — the
     * group stayed without push indefinitely. Idempotent + race-safe
     * (shared per-group in-flight lock with the bootstrap sweep).
     * Fire-and-forget; best-effort. No-op for an unknown group.
     */
    public fun ensurePushRouting(groupIdHex: String) {
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        ensurePushRoutingBestEffort(record.group, identity)
    }

    /**
     * ADR-0042 — re-broadcast the CURRENT held secret to [groupIdHex] (used
     * right after an add-member commit so the joiner learns it on their next
     * poll). No-op when this device holds no secret. Fire-and-forget on the
     * push scope; best-effort.
     */
    private fun redistributePushRoutingBestEffort(groupIdHex: String) {
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return
        pushScope.launch {
            try {
                GroupPushRouting.redistribute(
                    groupIdHex,
                    pushRoutingPorts(record.group, identity),
                )
            } catch (_: Throwable) {
                // best-effort
            }
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
            record.applyPollResult(
                result,
                callSignalingSink = { payload ->
                    callSignalingSink?.invoke(groupIdHex, payload)
                },
                pushRoutingSecretSink = { secret, senderLeaf ->
                    // ADR-0042 — gate on the MLS-authenticated sender leaf
                    // (only the ADR-0022 owner may CHANGE the secret), then
                    // persist + subscribe under the derived opaque `push_gid`.
                    // Fire-and-forget on the push scope so the poll loop is
                    // never blocked on the network subscribe; best-effort.
                    receivePushRoutingSecretBestEffort(
                        record.group,
                        identity,
                        secret,
                        senderLeaf,
                    )
                },
            )
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
            // ADR-0040 (G-35) — purge the durable pending-commit
            // outbox so a prior session's un-acked commits never
            // get swept into a later one on this device.
            try {
                CommitOutboxStore.deleteForAccount(applicationContext, accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
            // ADR-0042 — wipe the push-routing secrets + the push_gid reverse
            // index. The secret is encrypted at rest, but a stale reverse-index
            // row (plaintext push_gid -> group_id) would leave push-routing
            // residue behind — full per-account reset, mirroring the web
            // `clearAllGroupRouting` on forgetDevice.
            try {
                PushRoutingSecretStore.clearForAccount(applicationContext, accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
            try {
                pushRouteStore.clearForAccount(accountIdB64)
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
     *  mutation will retry the persist naturally.
     *
     *  Only safe on the RECEIVE / non-critical mutation paths
     *  (poll, group create/join, add-member, external-join
     *  approve). It MUST NOT be used on the SEND path — see
     *  [persistSnapshotDurableOrThrow]: publishing a gen-N
     *  ciphertext without its N+1 ratchet state durably on disk
     *  re-opens the AEAD nonce-reuse-on-crash window. */
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

    /**
     * Persist the canonical Identity's MLS provider snapshot
     * DURABLY and RE-THROW on any failure — the fail-closed
     * counterpart of [persistSnapshotBestEffort], used on the
     * SEND path BEFORE the DS publish (threaded into
     * [ChatClient.sendMessage] as its `persistRatchet` hook).
     *
     * SECURITY (nonce-reuse-on-crash): `createApplicationMessage`
     * consumes the current MLS sender-ratchet generation N and
     * leaves the in-memory OpenMLS provider at N+1. That N+1
     * state must be durably on disk BEFORE the gen-N ciphertext
     * reaches the DS. If this persist were swallowed (as
     * [persistSnapshotBestEffort] does) a crash between the
     * publish and a later persist would reload the ratchet at N,
     * and the next send would re-consume generation N — an
     * identical AEAD (key, nonce) on a different plaintext
     * (C1 XOR C2 = P1 XOR P2 plaintext leak to the zero-trust
     * server + Poly1305 tag forgery). So this variant AWAITs the
     * Keystore-wrapped write and THROWS on failure; the caller
     * ([ChatClient.sendMessage]) aborts the send before
     * publishing, so the gen-N ciphertext never leaves the device
     * and generation N stays safe to re-consume after a reload. A
     * crash in the new window (after this persist, before the
     * publish) merely SKIPS generation N — RFC 9420 receivers
     * tolerate sender-ratchet gaps, so there is no reuse either
     * way.
     *
     * Unlike [persistSnapshotBestEffort] this persists ONLY the
     * MLS ratchet snapshot — the plaintext message log stays a
     * best-effort AFTER-publish write
     * ([persistMessageLogBestEffort]), because it is not
     * security-critical and needs the DS messageId that only
     * exists post-publish.
     *
     * @throws IllegalStateException if the session has no
     *         canonical Identity / account id — cannot persist,
     *         so fail closed rather than silently skip.
     * @throws java.io.IOException / GeneralSecurityException /
     *         FfiException surfaced by the snapshot + Keystore
     *         write.
     */
    private fun persistSnapshotDurableOrThrow() {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "cannot persist MLS snapshot — no canonical Identity",
            )
        val accountIdB64 = currentAccountIdB64.get()
            ?: throw IllegalStateException(
                "cannot persist MLS snapshot — no account id",
            )
        val blob = identity.snapshotStorage()
        snapshotStore.putSnapshot(applicationContext, accountIdB64, blob)
    }

    // -----------------------------------------------------------------
    // ADR-0040 (G-35) — durable pending-commit outbox helpers
    // -----------------------------------------------------------------

    /** A fresh random id for one outbox entry (32 hex chars = 16 bytes). */
    private fun newOutboxEntryId(): String =
        java.util.UUID.randomUUID().toString().replace("-", "")

    /** The current session's base64 account id, or FAIL-CLOSED. A commit path
     *  cannot durably record an outbox entry without the account scope, so a
     *  missing id aborts the op (before any publish) rather than silently
     *  skipping the durability guarantee. */
    private fun requireAccountId(): String =
        currentAccountIdB64.get()
            ?: throw IllegalStateException(
                "cannot record pending commit — no account id",
            )

    /** Mark an outbox entry acked after BOTH artifacts have published.
     *  Best-effort: a failure here just leaves the entry un-acked, so the next
     *  restart sweep re-publishes it idempotently (peers drop the wrong-epoch
     *  duplicate; the committer's echo filter is re-armed). */
    private fun ackCommitBestEffort(id: String, commitMessageId: Long, secondMessageId: Long?) {
        try {
            CommitOutboxStore.markAcked(
                applicationContext,
                id,
                commitMessageId,
                secondMessageId,
            )
        } catch (_: Throwable) {
            // best-effort — the next sweep reconciles.
        }
    }

    /**
     * ADR-0040 F1 (lakko-security) — the persist-abort handler [runCrashSafeCommit]
     * invokes when a commit path's durable write throws (BEFORE any publish). Two
     * steps, in order:
     *
     *   1. **best-effort** drop the just-written outbox [entryId]. `publish` never
     *      ran, so the commit provably never reached the DS; deleting the entry
     *      keeps the post-abort durable state "N, no entry" (matching the web
     *      atomic-transaction reference). A leftover entry is ALSO safe — the
     *      restart sweep DISCARDs it via [reconcileVerdict] (snapshot at N below
     *      the entry's N+1 targetEpoch) — so a delete failure is swallowed.
     *   2. **fail-safe (hard)** reset the in-memory group to the durable snapshot
     *      (N) via [resetGroupToDurableSnapshotOrThrow]. A throw here is the
     *      poisoned-group signal — [runCrashSafeCommit] surfaces it as the hard
     *      error (never masks it).
     */
    private fun onCommitPersistAbort(groupIdHex: String, entryId: String) {
        try {
            CommitOutboxStore.deleteEntry(applicationContext, entryId)
        } catch (_: Throwable) {
            // best-effort — the restart sweep DISCARDs a torn orphan entry anyway.
        }
        resetGroupToDurableSnapshotOrThrow(groupIdHex)
    }

    /**
     * ADR-0040 F1 (lakko-security) — reload the in-memory MLS state for [groupIdHex]
     * from the durable snapshot, dropping the un-published eager merge.
     *
     * The core create-commit call eagerly merged the OpenMLS provider N -> N+1
     * before the (now-failed) durable persist, so the in-memory provider + the
     * cached [Group] handle sit at the unpublished N+1 while the durable snapshot
     * on disk is still at N (its write failed / never ran). [Identity.restoreStorage]
     * overwrites the whole provider from that N snapshot, then [loadGroup] rebuilds
     * a fresh handle reading the restored (N) provider, which replaces the cached
     * reference — so a subsequent same-session commit builds on N.
     *
     * Whole-provider restore note: `restoreStorage` is provider-wide (there is no
     * per-group restore in the FFI). In the normal case the durable snapshot equals
     * every OTHER group's current in-memory state (best-effort receive-path persists
     * succeed), so only THIS group is rolled back; a group whose prior best-effort
     * persist silently failed could be rolled back too — a pre-existing best-effort
     * receive-path edge, out of scope for this commit-path fix, and self-healing on
     * that group's next successful persist / poll.
     *
     * FAIL-SAFE: if the durable snapshot cannot be loaded, or has no such group,
     * this THROWS a hard [ChatError] rather than leaving the eager-merged (N+1)
     * handle in place — surfacing a "restart to recover" error to the caller. A
     * restart then re-bootstraps cleanly from the durable snapshot (at N). "Should
     * not happen" for an existing committed group, which always has a durable
     * snapshot.
     *
     * Runs under [groupsLock] (every commit call site holds it), so the provider
     * restore + handle swap are serialised against all other MLS ops.
     */
    private fun resetGroupToDurableSnapshotOrThrow(groupIdHex: String) {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "cannot reset group after commit-abort — no canonical Identity",
            )
        val accountIdB64 = currentAccountIdB64.get()
            ?: throw IllegalStateException(
                "cannot reset group after commit-abort — no account id",
            )
        val record = groups[groupIdHex]
            ?: throw IllegalStateException(
                "cannot reset group after commit-abort — no live handle for $groupIdHex",
            )
        // The durable snapshot is still at N (the commit's persist failed), so
        // restoring the provider from it DROPS the eager-merged N+1 for this group.
        val blob = snapshotStore.getSnapshot(applicationContext, accountIdB64)
            ?: throw ChatError(
                "group state poisoned after commit-abort — no durable snapshot to " +
                    "reset from; restart to recover",
                IllegalStateException("missing durable snapshot for account"),
            )
        identity.restoreStorage(blob)
        val fresh = loadGroup(identity, record.groupId)
            ?: throw ChatError(
                "group state poisoned after commit-abort — durable snapshot has no " +
                    "group $groupIdHex; restart to recover",
                IllegalStateException("loadGroup returned null after restore"),
            )
        // Swap the fresh (N) handle in; the record keeps its timeline + cursor.
        record.replaceGroup(fresh)
    }

    /**
     * ADR-0040 (G-35) — restart / group-open sweep. Re-publishes every un-acked
     * pending-commit outbox entry for [groupIdHex] IDEMPOTENTLY, re-arming the
     * committer's own-echo filter ([ChatClient.markOwnPublishedCommit]) for the
     * re-published commit, and DISCARDING a torn-write orphan (an un-acked entry
     * whose restored snapshot did not advance to the entry's `targetEpoch` — the
     * commit was never published; see [reconcileVerdict]). MUST run BEFORE the
     * first [pollGroup] so the re-armed messageId is in place when the poll
     * re-fetches the commit — else the poll would feed the device its own commit
     * and hit `CannotDecryptOwnMessage` (a benign drop-and-continue in
     * [ChatClient.pollMessages], but the re-arm avoids the round-trip).
     *
     * Idempotent + safe to call repeatedly: acked entries are GC-deleted, a
     * re-publish failure (offline DS) leaves the entry un-acked for the next
     * sweep, and a duplicate re-publish is dropped by peers (wrong epoch) and by
     * the committer (echo filter).
     */
    public suspend fun sweepPendingCommits(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val record = groups[groupIdHex] ?: return
        val groupIdB64 = outboxB64(record.groupId)
        val entries = try {
            CommitOutboxStore.loadForGroup(applicationContext, accountIdB64, groupIdB64)
        } catch (_: Throwable) {
            return
        }
        if (entries.isEmpty()) return
        groupsLock.withLock {
            val currentEpoch = try {
                record.group.epoch()
            } catch (_: Throwable) {
                return@withLock
            }
            sweepPendingCommitEntries(
                entries = entries,
                currentEpoch = currentEpoch,
                republishCommit = { commit -> dsClient.publishGroup(record.groupId, commit).id },
                republishRendezvous = { bytes -> dsClient.publishGroup(record.groupId, bytes).id },
                republishWelcome = { mailbox, bytes -> dsClient.publishMailbox(mailbox, bytes).id },
                markOwnPublished = { id -> chatClient.markOwnPublishedCommit(id) },
                onAck = { id, commitId, secondId -> ackCommitBestEffort(id, commitId, secondId) },
                onDiscard = { id ->
                    try {
                        CommitOutboxStore.deleteEntry(applicationContext, id)
                    } catch (_: Throwable) {
                        // best-effort GC / discard — the next sweep retries.
                    }
                },
            )
        }
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
    group: Group,
    val groupId: ByteArray,
) {
    /** Live MLS [Group] handle. `private set` — swapped only via [replaceGroup]
     *  (ADR-0040 F1) after a commit persist-abort resets the in-memory provider to
     *  the durable snapshot; otherwise stable for the record's lifetime. */
    var group: Group = group
        private set

    /**
     * ADR-0040 F1 (lakko-security) — swap in a freshly-[loadGroup]ed handle after a
     * commit persist-abort reset the in-memory MLS provider to the durable snapshot
     * (N). Preserves this record's timeline / cursor / typing / read-watermark
     * state (only the MLS handle is replaced) and closes the stale (eager-merged
     * N+1) handle to release its Rust-side Arc.
     */
    fun replaceGroup(fresh: Group) {
        val old = group
        if (old === fresh) return
        group = fresh
        try {
            old.close()
        } catch (_: Throwable) {
            // best-effort — releasing the stale Rust Arc must not mask the reset.
        }
    }

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
        pushRoutingSecretSink: ((ByteArray, UInt?) -> Unit)? = null,
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
                is IncomingChatEvent.PushRoutingSecret -> {
                    // ADR-0042 — a push-routing-secret distribution envelope.
                    // Not a timeline row; the repository gates on the sender
                    // leaf (only the ADR-0022 owner may CHANGE the secret) then
                    // persists + subscribes under the derived opaque `push_gid`.
                    // Handed to the sink (which fires the async receive on the
                    // push scope) together with the MLS-authenticated sender
                    // leaf the gate needs.
                    pushRoutingSecretSink?.invoke(event.secret, event.senderLeaf)
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
