// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import java.util.Collections
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
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
import uniffi.lakko_ffi_uniffi.OutgoingOwner
import uniffi.lakko_ffi_uniffi.ProposalRecord
import uniffi.lakko_ffi_uniffi.Role
import uniffi.lakko_ffi_uniffi.RoleTable
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.loadGroup

/**
 * G-37 — how many groups [ChatRepository.pollAllGroups] polls at once.
 *
 * The bound is NOT arbitrary and NOT about CPU. Every DS long poll goes to the
 * SAME host, and Ktor's OkHttp engine dispatches through `Call.enqueue`
 * (verified by decompiling `ktor-client-okhttp-jvm-3.0.1`: `OkUtilsKt.execute`
 * constructs an `OkHttpCallback` and invokes `Call.enqueue`), so OkHttp's
 * `Dispatcher.maxRequestsPerHost` applies. That default is 5 (verified by
 * decompiling `okhttp-4.12.0`'s no-arg `Dispatcher()`: `maxRequests = 64`,
 * `maxRequestsPerHost = 5`).
 *
 * So the per-host slots are a genuinely scarce SHARED resource: fill all five
 * with 30 s idle long polls and every other request to the DS — including the
 * user's next send — sits in OkHttp's `readyAsyncCalls` queue until one of them
 * returns. Fanning out unbounded would trade a lock-contention problem for a
 * connection-starvation one, which is strictly worse: it would delay sends with
 * no lock involved at all, so no amount of lock narrowing could fix it.
 *
 * Three leaves at least two slots free at all times — one for the open chat
 * screen's own continuous long poll (which runs concurrently with a chat-list
 * poll) and one for an interactive publish — while still collapsing the
 * worst-case chat-list latency from `waitSecs * N` to `waitSecs * ceil(N / 3)`.
 */
private const val POLL_ALL_CONCURRENCY: Int = 3

/** Seconds-to-millis, for the one place a long-poll budget has to be honoured
 *  without an actual long poll behind it (ADR-0045 UX §6.2.8 item 1 — every
 *  group on this device has ended, so there is nothing to hold open). */
private const val MILLIS_PER_SECOND: Long = 1_000L

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
     * [groupLock] for the same-group operations; reads can be
     * lock-free via the snapshot returned by `Collections.unmodifiableMap`.
     *
     * NOTE: it is a `synchronizedMap`, so every ITERATION must hold the map's
     * own monitor (`synchronized(groups) { … }`). Single-key get/put are
     * already atomic. Before G-37 sprint 2 the repository-wide mutex made
     * unsynchronised iteration accidentally safe; per-group locking removes
     * that accident, so the iterations are explicit now.
     */
    private val groups: MutableMap<String, GroupRecord> =
        Collections.synchronizedMap(mutableMapOf())

    /**
     * G-37 — PER-GROUP coroutine mutexes for serialising operations on ONE
     * group, replacing the single repository-wide mutex whose own doc comment
     * anticipated exactly this ("swap in a `ConcurrentHashMap<String, Mutex>`").
     *
     * ## What the per-group lock guarantees
     *
     * For two operations on the SAME group it preserves the pre-existing
     * ordering EXACTLY: the whole operation — create-commit, durable record,
     * AND the DS publish — runs inside it, so commit A is on the wire before
     * commit B is even CREATED. That ordering is load-bearing: a commit built
     * on an epoch peers have not reached is the "device ahead" strand ADR-0040
     * exists to prevent.
     *
     * Across DIFFERENT groups that ordering has no meaning, so they no longer
     * queue behind each other's network I/O — which is the whole point.
     *
     * `getOrPut` on a `ConcurrentHashMap` selects the `ConcurrentMap` overload
     * (backed by `putIfAbsent`), so racing callers converge on ONE `Mutex` per
     * key. Same primitive + same reasoning as
     * [me.lakko.app.push.GroupPushRouting]'s per-`(account, group)` routing lock.
     *
     * Entries are never evicted: one `Mutex` per group the session has touched
     * is bounded by the group count and cleared with the rest of the state in
     * [wipeInMemoryState].
     *
     * Lives in [GroupLocks] rather than as a bare map field so the per-group
     * property it provides can be driven by a host test — see `SplitPoll.kt`.
     */
    private val groupLocks: GroupLocks = GroupLocks()

    /** The [groupLocks] entry for [groupIdHex], creating it on first use. */
    private fun groupLock(groupIdHex: String): Mutex = groupLocks.forGroup(groupIdHex)

    /**
     * G-37 — the GLOBAL mutex guarding the ONE shared OpenMLS provider.
     *
     * ## Why a per-group lock is not sufficient on its own
     *
     * Every group's MLS state lives in a SINGLE `OpenMlsRustCrypto` provider
     * owned by the canonical [Identity], and the FFI offers no per-group
     * persistence: [Identity.snapshotStorage] serialises the WHOLE provider and
     * [Identity.restoreStorage] replaces the WHOLE provider (there is one
     * snapshot blob per ACCOUNT, not per group). So two provider-wide hazards
     * survive any amount of per-group locking:
     *
     *  - `snapshotStorage` takes a single read lock over the provider's storage
     *    map, while one logical MLS operation performs SEVERAL separate writes
     *    into it. A snapshot taken by group A part-way through group B's
     *    multi-write operation would capture B TORN, and that torn blob is what
     *    a later restore / restart would load.
     *  - worse, `restoreStorage` — the ADR-0040 F1 abort path
     *    ([resetGroupToDurableSnapshotOrThrow]) — replaces every group's state
     *    at once. Running it while another group sits at an un-persisted N+1
     *    would ROLL THAT GROUP BACK, re-opening the exact nonce-reuse /
     *    device-behind window ADR-0040 closes.
     *
     * Both were impossible before only because ONE mutex covered every group.
     * This lock keeps them impossible: every provider-touching region takes it,
     * so a whole-provider snapshot / restore can never overlap another group's
     * MLS work, and every other group is provably OUTSIDE its provider region
     * (hence in-memory == durable) whenever a restore runs.
     *
     * ## Where the concurrency comes from
     *
     * The network is deliberately NOT inside this lock. Each path splits into a
     * provider half and a network half — [ChatClient.prepareSend] /
     * [ChatClient.publishPreparedSend], [prepareCrashSafeCommit] / publish,
     * [ChatClient.fetchGroupJournal] / [ChatClient.applyFetchedMessages] — so
     * what different groups now serialise on is local CPU plus one snapshot
     * write, never a 15 s DS publish or a 30 s long poll.
     *
     * ## Lock ordering
     *
     * ALWAYS [groupLock] first, [providerLock] second. Never the reverse, and
     * never two [groupLock]s at once. Push-routing work is always LAUNCHED onto
     * [pushScope] rather than awaited under a group lock, so
     * `GroupPushRouting`'s own routing lock stays strictly OUTSIDE this one
     * (routing -> provider), exactly as it was against the old repository-wide
     * mutex.
     */
    private val providerLock = Mutex()

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

        // G-37 — the group does not exist yet, so NO other operation can be
        // targeting it and there is no per-group lock to take until its id
        // exists. Only the shared provider needs guarding here.
        val created = providerLock.withLock {
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
            CreatedGroup(
                group = group,
                groupId = group.groupId(),
                recipientPseudonym = recipientPseudonym,
                welcomeEnvelope = welcomeEnvelope,
            )
        }

        // G-37 — the DS mailbox publish runs OUTSIDE the provider lock. Its
        // position relative to the registration + persist below is unchanged: a
        // publish failure still throws before the group is registered or
        // persisted, exactly as when one mutex covered the whole block.
        try {
            dsClient.publishMailbox(created.recipientPseudonym, created.welcomeEnvelope)
        } catch (e: Throwable) {
            throw ChatError("DS publishMailbox (Welcome) failed", e)
        }

        val groupIdHex = DsClient.pseudonymHex(created.groupId)
        groupLock(groupIdHex).withLock {
            providerLock.withLock {
                groups[groupIdHex] =
                    GroupRecord(group = created.group, groupId = created.groupId)
                persistKnownGroupBestEffort(groupIdHex)
                persistSnapshotBestEffort()
                // ADR-0042 — the founder is the OWNER: mint the push-routing
                // secret + distribute it + subscribe under the derived push_gid.
                // G-37 — kept INSIDE the provider critical section because it
                // reads `group.ownRole` / `group.groupId` synchronously, exactly
                // as it did under the old repository-wide mutex. The actual
                // routing work is LAUNCHED onto `pushScope`, so this nests no
                // locks: the launched body takes the routing lock and only then
                // the provider lock, never while this one is held.
                ensurePushRoutingBestEffort(created.group, identity)
            }
            publishThreadList()
        }
        return groupIdHex
    }

    /** G-37 — carries a freshly-created group across the provider-lock release
     *  so the Welcome mailbox publish can run outside it. */
    private class CreatedGroup(
        val group: Group,
        val groupId: ByteArray,
        val recipientPseudonym: ByteArray,
        val welcomeEnvelope: ByteArray,
    )

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
        // G-37 — provider-only (no DS call in this path), and the group id is
        // not known until the join has run, so the provider lock alone covers
        // it. Nothing else can be operating on a group this device is only now
        // learning about.
        val groupIdHex = providerLock.withLock {
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
            // ADR-0042 — a joiner is not the owner: `ensure` defers the
            // subscription (never under group_id) until the owner's
            // distribution envelope arrives on the journal, unless this
            // device already holds the secret (e.g. a prior receive).
            // G-37 — kept INSIDE the provider critical section: it reads
            // `group.ownRole` / `group.groupId` synchronously and only LAUNCHES
            // the routing work, so no locks nest here.
            ensurePushRoutingBestEffort(group, identity)
            groupIdHex
        }
        publishThreadList()
        return groupIdHex
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
        // G-37 — pure provider reads (MLS exporter + own leaf), no network.
        return providerLock.withLock {
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
     * Reads are lock-free under [groupLock] semantics (a concurrent
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
     * ADR-0023 — our own IDENTITY in [groupIdHex]: this device's signature key,
     * base64, resolved LIVE through the roster. `null` when there is no handle
     * or the roster could not be read.
     *
     * The UI's own-side authorship decisions read this rather than
     * [ownLeafIndex]: a leaf is a position, and a position we once held can be
     * handed to a member added later, at which point a leaf comparison starts
     * calling a stranger "us". Resolved rather than stored for the same reason.
     */
    public fun ownMemberIdentity(groupIdHex: String): MemberIdentity? {
        val record = groups[groupIdHex] ?: return null
        return ownIdentity(record.group)
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
    ): MembershipOutcome {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        // G-37 — per-group lock spans the WHOLE op (create-commit, durable
        // record, publish), so two role changes on the SAME group still
        // serialise exactly as before. The provider lock covers only the
        // provider-touching prefix; the publish runs outside it.
        return groupLock(groupIdHex).withLock {
            val publishStep = providerLock.withLock {
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
                // ADR-0045 obligation 5 — a role change is a membership commit
                // like any other, so it proposes too. A group reaching this
                // point necessarily HAS a role table (it was just read above),
                // so unlike add / remove there is no direct-path fallback to
                // preserve — but the branch is still written explicitly rather
                // than assumed, because `usesProposalPath` is the one predicate
                // that decides this and it should be asked, not inferred.
                if (usesProposalPath(record.group)) {
                    // C-65 — the OTHER direction of the collision an ownership
                    // transfer already pre-checks. A transfer IS a role-table
                    // rewrite, so it occupies the same exclusive
                    // `GroupContextExtensions` slot as this toggle and the two
                    // exclude each other both ways. Without this arm an owner
                    // who taps `Make admin` while their own transfer is waiting
                    // gets the core's diagnostic naming two Rust functions —
                    // the exact failure the seventh refusal string exists to
                    // replace, reached from the one side nobody checked.
                    //
                    // A torn read is treated as "no", the same fail-open the
                    // transfer path takes and for the same reason: the core is
                    // the enforcement, and this check only chooses which
                    // sentence the owner reads.
                    val blocked = try {
                        hasPendingRoleChange(record.group.pendingProposals())
                    } catch (_: Throwable) {
                        false
                    }
                    if (blocked) {
                        throw ChatError(
                            ROLE_CHANGE_ALREADY_PENDING,
                            IllegalStateException("C-65: a role change is already pending"),
                        )
                    }
                    val proposalBytes = try {
                        record.group.proposeGroupRoles(identity, table.owner, admins)
                    } catch (e: FfiException) {
                        throw ChatError("proposeGroupRoles failed", e)
                    }
                    persistSnapshotBestEffort()
                    return@withLock suspend {
                        val publish = try {
                            dsClient.publishGroup(record.groupId, proposalBytes)
                        } catch (e: Throwable) {
                            throw ChatError("DS publishGroup (role-change proposal) failed", e)
                        }
                        noteOwnRequestBestEffort(groupIdHex, "role-change")
                        publishThreadList()
                        MembershipOutcome.Proposed(publish.id) as MembershipOutcome
                    }
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
                prepareCrashSafeCommit(
                    persistEntryOrThrow = { CommitOutboxStore.putEntry(applicationContext, entry) },
                    persistSnapshotOrThrow = { persistSnapshotDurableOrThrow() },
                    // ADR-0040 F1 (lakko-security) — on a fail-closed persist-abort,
                    // reset the eager-merged in-memory group (N+1) back to the durable
                    // snapshot (N) so a subsequent same-session commit builds on N, not
                    // the unpublished N+1. Fires ONLY on the persist-abort, never on a
                    // publish failure (that commit is durably recorded — the sweep
                    // re-publishes it). See [onCommitPersistAbort].
                    onPersistAbort = { onCommitPersistAbort(groupIdHex, entry.id) },
                )
                // Returning normally means BOTH durable writes landed, so the
                // commit is now safe to put on the wire — the ADR-0040 §2 ordering,
                // unchanged, just expressed across the lock release.
                suspend {
                    val commitPublish = try {
                        dsClient.publishGroup(record.groupId, commitBytes)
                    } catch (e: Throwable) {
                        throw ChatError("DS publishGroup (role change) failed", e)
                    }
                    chatClient.markOwnPublishedCommit(commitPublish.id)
                    ackCommitBestEffort(entry.id, commitPublish.id, null)
                    publishThreadList()
                    MembershipOutcome.Committed(commitPublish.id, null) as MembershipOutcome
                }
            }
            publishStep()
        }
    }

    /**
     * ADR-0045 UX §6.1 — hand the group's owner slot to another member.
     *
     * **Always the proposal arm, and there is no second one here.** The UniFFI
     * surface carries `transferOwnership` too, and this method deliberately
     * never reaches it, because for this flow the direct arm is *structurally
     * unreachable*: the UI gates the control on `isOwner`, read off the role
     * table ([ownRole]), and [usesProposalPath] is the identical `groupRoles()`
     * read. A group with no role table has no owner, so
     * `isOwner && !usesProposalPath` cannot hold. That makes transfer the one
     * membership action needing a single arm rather than two, unlike add /
     * remove / role change, each of which carries a direct fallback for the
     * table-less case. A table-less group reaching here anyway is refused by the
     * core, which is the right place for it.
     *
     * Everything else mirrors [setMemberAdmin]'s proposal arm exactly, because a
     * transfer **is** a role change — deliberately not a new commit type, just
     * the owner-only role-table rewrite with the owner slot moved. So it
     * inherits that arm's discipline verbatim: the per-group lock, the provider
     * lock over the provider-touching prefix only, **no outbox entry and no
     * reset region** (a proposal advances no epoch, so there is nothing to roll
     * back), and a [MembershipOutcome.Proposed] the caller sweeps immediately.
     *
     * The one thing beyond that arm's shape is the observation record's
     * SUCCESSOR KEY (§6.1.10), which is the only thing that later distinguishes
     * *our pending role change is a transfer* from *our pending role change is a
     * promote* — `ProposalKind::RoleChange` carries no payload — and which is
     * what names the person in `Waiting to make {name} the owner.`
     *
     * [newOwnerSignaturePubkey] is the successor's 32-byte Ed25519 signature
     * public key — the stable handle [members] reports and the role table keys
     * on — never a leaf index: OpenMLS hands a vacated leaf to the next Add.
     *
     * [outgoing] decides what becomes of THIS device's own role and is required.
     * The core refuses to default it and the FFI carries that structurally (no
     * `Default` on the generated enum), so this signature keeps the property by
     * taking a plain non-nullable parameter with no default value.
     *
     * The immediate sweep is the CALLER's, not this method's: [sweepIfCommitter]
     * takes the same per-group lock and a `Mutex` is not reentrant, so nesting
     * it here would hang. The ViewModel calls the two in turn.
     */
    public suspend fun transferGroupOwnership(
        groupIdHex: String,
        newOwnerSignaturePubkey: ByteArray,
        outgoing: OutgoingOwner,
    ): MembershipOutcome {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        // G-37 — per-group lock spans the WHOLE op; the provider lock covers
        // only the provider-touching prefix, so the publish runs outside it.
        return groupLock(groupIdHex).withLock {
            val publishStep = providerLock.withLock {
                val record = groups[groupIdHex]
                    ?: throw ChatError(
                        "no live Group handle for $groupIdHex",
                        IllegalStateException("missing group record"),
                    )
                // C-65, and it collides in BOTH directions: a pending role
                // change refuses a transfer, and a pending transfer refuses a
                // role change. It falls out of `propose_ownership_transfer`
                // delegating to `propose_group_roles` rather than being
                // restated, which is the point. The UI disables the control with
                // this same string as its reason once the blocking request is
                // past the quiet window (UX §6.1.8's deliberate asymmetry with
                // the admin toggles, which keep refuse-on-tap); THIS is the
                // check for the window in which the control is still live.
                //
                // A torn read is treated as "no", which lets the request through
                // to the core's own authorisation rather than refusing on a
                // transient provider failure — the core is the enforcement
                // either way, and this check exists only to convert a message
                // naming two Rust functions into one the owner can act on.
                val blocked = try {
                    hasPendingRoleChange(record.group.pendingProposals())
                } catch (_: Throwable) {
                    false
                }
                if (blocked) {
                    throw ChatError(
                        ROLE_CHANGE_ALREADY_PENDING,
                        IllegalStateException("C-65: a role change is already pending"),
                    )
                }
                val proposalBytes = try {
                    record.group.proposeOwnershipTransfer(
                        identity,
                        newOwnerSignaturePubkey,
                        outgoing,
                    )
                } catch (e: FfiException) {
                    throw ChatError("proposeOwnershipTransfer failed", e)
                }
                // The proposal is now in this device's own pending store, which
                // is provider state — persist so a relaunch does not lose a
                // request the peers hold.
                persistSnapshotBestEffort()
                suspend {
                    val publish = try {
                        dsClient.publishGroup(record.groupId, proposalBytes)
                    } catch (e: Throwable) {
                        throw ChatError("DS publishGroup (ownership-transfer proposal) failed", e)
                    }
                    noteOwnRequestBestEffort(
                        groupIdHex,
                        "role-change",
                        transferToB64 = outboxB64(newOwnerSignaturePubkey),
                    )
                    publishThreadList()
                    MembershipOutcome.Proposed(publish.id) as MembershipOutcome
                }
            }
            publishStep()
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
    ): MembershipOutcome {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        val keyPackageBytes = discovery.fetchKeyPackage(recipientAccountId)
            ?: throw ChatError(
                "recipient has no KeyPackage uploaded yet",
                IllegalStateException("fetchKeyPackage returned null"),
            )
        // G-37 — per-group lock spans the WHOLE op (create-commit, durable
        // record, both publishes), so two membership commits on the SAME group
        // still serialise exactly as before: A is on the wire before B is even
        // created. The provider lock covers only the provider-touching prefix.
        val outcome = groupLock(groupIdHex).withLock {
            val publishStep = providerLock.withLock {
                val record = groups[groupIdHex]
                    ?: throw ChatError(
                        "no live Group handle for $groupIdHex",
                        IllegalStateException("missing group record"),
                    )
                // ADR-0045 obligation 5 — a group carrying an ADR-0022 role
                // table routes membership through PROPOSALS. The direct path
                // below stays byte-for-byte for a group with NO role table,
                // which has no committer and whose peers' receipt gates would
                // drop a proposal outright.
                //
                // The core enforces the split from the other side too:
                // `addMemberFromWire` REFUSES while anything is pending, because
                // every OpenMLS entry point the direct paths are built on sweeps
                // the WHOLE pending set alongside the one change it was asked for
                // — an admin asking to add one member would add two, and route
                // the single Welcome to only one of them.
                if (usesProposalPath(record.group)) {
                    val proposalBytes = try {
                        record.group.proposeAddMemberFromWire(identity, keyPackageBytes)
                    } catch (e: FfiException) {
                        throw ChatError("proposeAddMemberFromWire failed", e)
                    }
                    // The proposal is now in this device's own pending store,
                    // which is provider state — persist so a relaunch does not
                    // lose a request the peers hold.
                    persistSnapshotBestEffort()
                    return@withLock suspend {
                        val publish = try {
                            dsClient.publishGroup(record.groupId, proposalBytes)
                        } catch (e: Throwable) {
                            throw ChatError("DS publishGroup (add-member proposal) failed", e)
                        }
                        noteOwnRequestBestEffort(groupIdHex, "add:" + outboxB64(recipientSignaturePubkey))
                        publishThreadList()
                        MembershipOutcome.Proposed(publish.id) as MembershipOutcome
                    }
                }
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
                prepareCrashSafeCommit(
                    persistEntryOrThrow = { CommitOutboxStore.putEntry(applicationContext, entry) },
                    persistSnapshotOrThrow = { persistSnapshotDurableOrThrow() },
                    // ADR-0040 F1 (lakko-security) — on a fail-closed persist-abort,
                    // reset the eager-merged in-memory group (N+1) back to the durable
                    // snapshot (N) so a subsequent same-session commit builds on N, not
                    // the unpublished N+1. Fires ONLY on the persist-abort, never on a
                    // publish failure (that commit is durably recorded — the sweep
                    // re-publishes it). See [onCommitPersistAbort].
                    onPersistAbort = { onCommitPersistAbort(groupIdHex, entry.id) },
                )
                // Returning normally means BOTH durable writes landed, so the
                // commit is now safe to put on the wire — the ADR-0040 §2 ordering,
                // unchanged, just expressed across the lock release.
                suspend {
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
                    MembershipOutcome.Committed(commitPublish.id, welcomePublish.id) as MembershipOutcome
                }
            }
            publishStep()
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
        // ADR-0045 — only when the ADD actually LANDED. On the proposal path
        // nobody has been added yet, so there is no new member to teach the
        // secret to; the redistribute is driven by whichever device sweeps the
        // proposal (see [sweepIfCommitter]).
        if (outcome is MembershipOutcome.Committed) redistributePushRoutingBestEffort(groupIdHex)
        return outcome
    }

    /**
     * Remove a member from an EXISTING group by leaf index. Publishes the
     * removal commit to the group journal (own-marked for echo suppression).
     * Returns the commit's DS message id.
     *
     * ADR-0043 / ADR-0044 (**C-61**) — the push-routing re-key is started
     * STRICTLY AFTER the Remove commit is published, and that ordering is
     * expressed by the [GroupPushRouting.publishRemoveThenRotate] seam
     * rather than by two adjacent statements. It is load-bearing twice
     * over:
     *
     *   - the distribution envelope is an MLS application message, so
     *     sealed at the PRE-removal epoch the member being evicted is
     *     still in that epoch and can OPEN it — learning the new secret
     *     and defeating the whole rotation. Sealed after, it is at the
     *     post-removal epoch, which RFC 9420 forward secrecy denies the
     *     removed leaf;
     *   - ADR-0044 — the rotation STAMPS the new secret with
     *     `Group.epoch()` read at mint time, so publishing first is also
     *     what makes it strictly out-rank the secret it replaces. Before
     *     the commit the stamp would be the PRE-removal epoch, which TIES
     *     with the secret the evicted member holds; the tie falls to the
     *     32 secret bytes and the rotation may simply LOSE, silently.
     *
     * Without a rotation at all the evicted device keeps deriving the
     * current `push_gid` and push-gw keeps waking it, so it learns "this
     * group was active at time T" on every send, forever — and nobody else
     * can stop it, since unsubscribe only ever deletes the caller's OWN
     * row.
     *
     * The rotation is fire-and-forget on the push scope: a failed rotation
     * must not unwind the removal, and a failed BROADCAST leaves the row
     * pending for the existing `ensure` re-broadcast branch.
     */
    public suspend fun removeMemberFromGroup(
        groupIdHex: String,
        leafIndex: UInt,
    ): MembershipOutcome {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        // ADR-0045 — on the PROPOSAL path nothing has been removed yet, so the
        // rotation must NOT fire: the envelope would be sealed at the
        // PRE-removal epoch, which the still-present target can open — the exact
        // C-61 violation the seam below exists to prevent, reached from the
        // other end. The branch cannot be taken before `publishRemove` runs
        // (`usesProposalPath` is a provider read and the authoritative branch is
        // taken under the group lock inside it), so it is recorded BY that call
        // and consulted by the rotate step — which `publishRemoveThenRotate`
        // invokes strictly afterwards, so the flag is always settled by then.
        // The removal that DOES land is rotated by whichever device sweeps it
        // (see [sweepIfCommitter]) or, for every other member, by the poll's
        // membership-shrink diff.
        var removalLanded = false
        val step = rotationStepOrNull(groupIdHex)
        return GroupPushRouting.publishRemoveThenRotate(
            scope = pushScope,
            // Owner-or-admin gated inside `GroupPushRouting.rotate`, which
            // also no-ops when this device holds no secret, so rotation
            // never becomes a second mint path.
            rotate = if (step == null) {
                null
            } else {
                suspend {
                    if (removalLanded) {
                        step()
                    } else {
                        GroupPushRouting.RotationOutcome.SKIP_NO_SHRINK
                    }
                }
            },
            onRotationError = { cause ->
                android.util.Log.w("ChatRepository", "push routing: rotation failed", cause)
            },
            publishRemove = {
                publishRemoveCommit(groupIdHex, identity, leafIndex).also {
                    removalLanded = it is MembershipOutcome.Committed
                }
            },
        )
    }

    /**
     * The Remove-commit half of [removeMemberFromGroup]: apply + durably
     * record + publish the commit, returning its DS message id. Split out
     * so the C-61 ordering can be expressed as one
     * [GroupPushRouting.publishRemoveThenRotate] call.
     */
    private suspend fun publishRemoveCommit(
        groupIdHex: String,
        identity: Identity,
        leafIndex: UInt,
    ): MembershipOutcome =
        // G-37 — per-group lock spans the WHOLE op; the provider lock covers
        // only the provider-touching prefix, so the publish is outside it.
        groupLock(groupIdHex).withLock {
            val publishStep = providerLock.withLock {
                val record = groups[groupIdHex]
                    ?: throw ChatError(
                        "no live Group handle for $groupIdHex",
                        IllegalStateException("missing group record"),
                    )
                // ADR-0045 obligation 5 — propose in a role-tabled group; the
                // direct commit below survives verbatim for a group with none.
                if (usesProposalPath(record.group)) {
                    val proposalBytes = try {
                        record.group.proposeRemoveMember(identity, leafIndex)
                    } catch (e: FfiException) {
                        throw ChatError("proposeRemoveMember failed", e)
                    }
                    persistSnapshotBestEffort()
                    return@withLock suspend {
                        val publish = try {
                            dsClient.publishGroup(record.groupId, proposalBytes)
                        } catch (e: Throwable) {
                            throw ChatError("DS publishGroup (remove-member proposal) failed", e)
                        }
                        noteOwnRequestBestEffort(groupIdHex, "remove:$leafIndex")
                        publishThreadList()
                        MembershipOutcome.Proposed(publish.id) as MembershipOutcome
                    }
                }
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
                prepareCrashSafeCommit(
                    persistEntryOrThrow = { CommitOutboxStore.putEntry(applicationContext, entry) },
                    persistSnapshotOrThrow = { persistSnapshotDurableOrThrow() },
                    // ADR-0040 F1 (lakko-security) — on a fail-closed persist-abort,
                    // reset the eager-merged in-memory group (N+1) back to the durable
                    // snapshot (N) so a subsequent same-session commit builds on N, not
                    // the unpublished N+1. Fires ONLY on the persist-abort, never on a
                    // publish failure (that commit is durably recorded — the sweep
                    // re-publishes it). See [onCommitPersistAbort].
                    onPersistAbort = { onCommitPersistAbort(groupIdHex, entry.id) },
                )
                // Returning normally means BOTH durable writes landed, so the
                // commit is now safe to put on the wire — the ADR-0040 §2 ordering,
                // unchanged, just expressed across the lock release. C-61 is
                // untouched: the rotation is started by
                // [GroupPushRouting.publishRemoveThenRotate] only after THIS
                // function returns, i.e. still strictly after the publish.
                suspend {
                    val commitPublish = try {
                        dsClient.publishGroup(record.groupId, commitBytes)
                    } catch (e: Throwable) {
                        throw ChatError("DS publishGroup (remove-member commit) failed", e)
                    }
                    chatClient.markOwnPublishedCommit(commitPublish.id)
                    ackCommitBestEffort(entry.id, commitPublish.id, null)
                    publishThreadList()
                    MembershipOutcome.Committed(commitPublish.id, null) as MembershipOutcome
                }
            }
            publishStep()
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
     * ADR-0027 (G-3), on the ADR-0045 proposal path since **S7** — approve a
     * pending external-join request (owner/admin only; the core role-gates it,
     * fail-closed).
     *
     * ## One artifact, and no epoch
     *
     * The core publishes an ordinary admin-authored `Add` **proposal** carrying
     * the joiner's retained KeyPackage — the admin's own MLS-authenticated
     * signature *is* the approval — so this posts exactly one thing to the group
     * journal and the epoch does not move. Everything the old direct-commit
     * shape needed is therefore gone: no Welcome to compute here, no
     * `WelcomeRendezvous` to post here, and no ADR-0040 outbox entry, because a
     * proposal makes nothing durable that a crash could strand.
     *
     * The joiner's Welcome — and its `WelcomeRendezvous` — is produced by
     * whichever device later sweeps this proposal into a commit. See
     * [runCommitterSweep]: that sweep posts one rendezvous per added key, blind
     * to which of them came from a link.
     *
     * ## No pre-drain, and no pending-set refusal
     *
     * Both are gone with the direct commit they existed for, and with them the
     * whole [runApproveExternalJoin] seam that sequenced them. The core's
     * `refuse_if_proposals_pending` no longer guards this path, so an approve
     * composes with whatever else is waiting — which matters, because under
     * ADR-0045 a non-empty pending set is the ORDINARY state and an admin
     * standing down behind a live owner could otherwise essentially never
     * approve.
     *
     * A refusal throws and leaves the request pending in the core, so the
     * Approve affordance stays and can be retried.
     *
     * G-37 — the per-group lock spans the whole op; the provider lock covers
     * only the provider-touching prefix, so the publish runs outside it.
     *
     * @return the proposal's DS message id.
     */
    public suspend fun approveExternalJoin(
        groupIdHex: String,
        proposalRef: ByteArray,
    ): Long {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        return groupLock(groupIdHex).withLock {
            val (groupId, proposalBytes) = providerLock.withLock {
                val record = groups[groupIdHex]
                    ?: throw ChatError(
                        "no live Group handle for $groupIdHex",
                        IllegalStateException("missing group record"),
                    )
                val bytes = try {
                    record.group.approveExternalJoin(identity, proposalRef)
                } catch (e: FfiException) {
                    throw ChatError("approveExternalJoin failed", e)
                }
                // Best-effort, like every other propose path: a proposal
                // advances no epoch, so a snapshot that does not land costs a
                // re-fetch of a journal row, never a desync.
                persistSnapshotBestEffort()
                Pair(record.groupId, bytes)
            }
            val publish = try {
                dsClient.publishGroup(groupId, proposalBytes)
            } catch (e: Throwable) {
                throw ChatError("DS publishGroup (approve proposal) failed", e)
            }
            chatClient.markOwnPublishedCommit(publish.id)
            // ADR-0045 S7 residual 1 — record the approval as OUR OWN request,
            // and flag where it came from. Only this device can know that: on
            // the wire an approved add is indistinguishable from an admin adding
            // a contact. If the sweep never carries it, that flag is what makes
            // the notice say `Ask them to request to join again` rather than
            // offering a retry this side cannot perform — a successful approve
            // CONSUMED the retained request, so the admin cannot even
            // re-approve.
            noteOwnRequestBestEffort(
                groupIdHex = groupIdHex,
                key = "add:" + outboxB64(proposalRef),
                joinRequest = true,
            )
            publishThreadList()
            publish.id
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
        // G-37 — local-only (no DS call): provider work start to finish.
        groupLock(groupIdHex).withLock {
            providerLock.withLock {
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
        groupLock(groupIdHex).withLock {
            // SECURITY (nonce-reuse-on-crash) — persist the
            // advanced (N+1) ratchet DURABLY before the DS
            // publish; a persist failure aborts the send inside
            // `chatClient.prepareSend` so no gen-N ciphertext
            // leaves the device. This path appends no local
            // bubble, so there is no after-publish message-log
            // persist to run.
            //
            // G-37 — the ratchet advance + durable persist run under the
            // provider lock; the DS publish runs outside it. The ordering is
            // unchanged: `prepareSend` must return before there is anything to
            // publish.
            val prepared = providerLock.withLock {
                chatClient.prepareSend(
                    identity = identity,
                    group = record.group,
                    plaintext = plaintextBytes,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
                )
            }
            // ADR-0045 — the publish is INSIDE the per-group lock (see
            // [runGroupSend]): a message sealed at one epoch and published on the
            // far side of a commit is permanently undecryptable for every
            // recipient, because `lakko-mls` retains no past-epoch secrets.
            chatClient.publishPreparedSend(prepared)
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

        groupLock(groupIdHex).withLock {
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
            // throws out of `chatClient.prepareSend` (before any
            // publish), surfacing to the ViewModel as a failed
            // send. The AFTER-publish persist below is the
            // best-effort plaintext message log only (it needs
            // the DS messageId that only exists post-publish) —
            // the security-critical ratchet snapshot already
            // landed durably above.
            //
            // G-37 — our own identity is resolved in the SAME provider
            // critical section as the ratchet advance, so the author stamp
            // cannot be taken across another group's commit.
            val (prepared, ownKey) = providerLock.withLock {
                val prepared = chatClient.prepareSend(
                    identity = identity,
                    group = record.group,
                    plaintext = envelopeBytes,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
                )
                // ADR-0023 — stamp our own signature key as the author so a
                // later edit/delete op of this row (ours or a peer's attempt)
                // is gated against the real author. NOT our leaf: any leaf,
                // ours included, can be reassigned to a member added later.
                val ownKey = ownIdentity(record.group)
                prepared to ownKey
            }
            val result = chatClient.publishPreparedSend(prepared)
            record.appendOutgoing(
                text = plaintext,
                attachments = attachments,
                messageId = result.messageId,
                createdAt = result.createdAt,
                authorKey = ownKey,
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
        groupLock(groupIdHex).withLock {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeEditEnvelope(
                target = targetMessageId,
                newText = newText,
            )
            // SECURITY (nonce-reuse-on-crash) — persist the (N+1)
            // ratchet durably, fail-closed, BEFORE publish.
            // G-37 — provider work under the provider lock, publish outside it.
            val (prepared, ownKey) = providerLock.withLock {
                val prepared = chatClient.prepareSend(
                    identity = identity,
                    group = record.group,
                    plaintext = envelopeBytes,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
                )
                // ADR-0023 — our own IDENTITY, resolved through the roster in
                // the same critical section as the ratchet advance. Never our
                // leaf: a leaf names a position, and the position moves.
                val ownKey = ownIdentity(record.group)
                prepared to ownKey
            }
            chatClient.publishPreparedSend(prepared)
            record.applyOwnOp(
                op = MessageOp.Edit,
                target = targetMessageId,
                text = newText,
                emoji = null,
                ownKey = ownKey,
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
        groupLock(groupIdHex).withLock {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeDeleteEnvelope(
                target = targetMessageId,
            )
            // SECURITY (nonce-reuse-on-crash) — persist the (N+1)
            // ratchet durably, fail-closed, BEFORE publish.
            // G-37 — provider work under the provider lock, publish outside it.
            val (prepared, ownKey) = providerLock.withLock {
                val prepared = chatClient.prepareSend(
                    identity = identity,
                    group = record.group,
                    plaintext = envelopeBytes,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
                )
                // ADR-0023 — our own IDENTITY, resolved through the roster in
                // the same critical section as the ratchet advance. Never our
                // leaf: a leaf names a position, and the position moves.
                val ownKey = ownIdentity(record.group)
                prepared to ownKey
            }
            chatClient.publishPreparedSend(prepared)
            record.applyOwnOp(
                op = MessageOp.Delete,
                target = targetMessageId,
                text = "",
                emoji = null,
                ownKey = ownKey,
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
        groupLock(groupIdHex).withLock {
            val envelopeBytes = uniffi.lakko_ffi_uniffi.composeReactionEnvelope(
                target = targetMessageId,
                emoji = emoji,
            )
            // SECURITY (nonce-reuse-on-crash) — persist the (N+1)
            // ratchet durably, fail-closed, BEFORE publish.
            // G-37 — provider work under the provider lock, publish outside it.
            val (prepared, ownKey) = providerLock.withLock {
                val prepared = chatClient.prepareSend(
                    identity = identity,
                    group = record.group,
                    plaintext = envelopeBytes,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
                )
                // ADR-0023 — our own IDENTITY, resolved through the roster in
                // the same critical section as the ratchet advance. Never our
                // leaf: a leaf names a position, and the position moves.
                val ownKey = ownIdentity(record.group)
                prepared to ownKey
            }
            chatClient.publishPreparedSend(prepared)
            record.applyOwnOp(
                op = MessageOp.Reaction,
                target = targetMessageId,
                text = "",
                emoji = emoji,
                ownKey = ownKey,
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
            groupLock(groupIdHex).withLock {
                // SECURITY (nonce-reuse-on-crash) — a typing tick
                // still advances the MLS sender ratchet, so
                // persist the (N+1) state durably, fail-closed,
                // BEFORE publish. A persist failure aborts the
                // publish (no gen-N ciphertext on the wire) and is
                // swallowed below — a dropped typing tick is
                // invisible (the receiver auto-expires anyway).
                // G-37 — provider work under the provider lock, publish outside.
                val prepared = providerLock.withLock {
                    chatClient.prepareSend(
                        identity = identity,
                        group = record.group,
                        plaintext = envelopeBytes,
                        persistRatchet = { persistSnapshotDurableOrThrow() },
                    )
                }
                chatClient.publishPreparedSend(prepared)
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
            groupLock(groupIdHex).withLock {
                // SECURITY (nonce-reuse-on-crash) — a read-receipt
                // still advances the MLS sender ratchet, so
                // persist the (N+1) state durably, fail-closed,
                // BEFORE publish. A persist failure aborts the
                // publish (no gen-N ciphertext on the wire) and is
                // swallowed below — the next watermark advance
                // re-publishes.
                // G-37 — provider work under the provider lock, publish outside.
                val prepared = providerLock.withLock {
                    chatClient.prepareSend(
                        identity = identity,
                        group = record.group,
                        plaintext = envelopeBytes,
                        persistRatchet = { persistSnapshotDurableOrThrow() },
                    )
                }
                chatClient.publishPreparedSend(prepared)
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
     * ADR-0043 — drop this device's push-gw subscription under a SUPERSEDED
     * [pushGid]. The FIRST call site [PushClient.unsubscribeGroup] has ever
     * had: the wrapper shipped with ADR-0042 but was dead code on all three
     * clients, which is what compounded G-36 residual 2 (an evicted member's
     * subscription was never torn down by anyone).
     *
     * The endpoint deletes exactly the `(device, group)` row named in the
     * request, and a client knows only its OWN device pseudonym — so this
     * can only ever retire THIS device's row, never a peer's.
     */
    private suspend fun unsubscribePushGidBestEffort(pushGid: ByteArray) {
        val pseudonym = DevicePushId.getOrCreate(applicationContext)
        PushClient().use { client ->
            client.unsubscribeGroup(
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
     * persist-before-publish control application message under [providerLock]).
     */
    private fun pushRoutingPorts(
        groupIdHex: String,
        group: Group,
        identity: Identity,
    ): PushRoutingPorts =
        NativePushRoutingPorts(
            context = applicationContext,
            accountIdB64 = currentAccountIdB64.get() ?: "",
            routeStore = pushRouteStore,
            subscribeFn = { pushGid -> subscribePushGidBestEffort(pushGid) },
            unsubscribeFn = { pushGid -> unsubscribePushGidBestEffort(pushGid) },
            broadcastFn = { envelope ->
                broadcastControlToGroup(identity, groupIdHex, group, envelope)
            },
            // ADR-0044 — the MINT stamp source. Read under [providerLock]
            // (the `Group` handle is not safe to touch off it, the same
            // discipline `broadcastControlToGroup` follows) and read
            // LAZILY, per call: `GroupPushRouting.rotate` invokes this
            // INSIDE its serializing rotation lock, after any queue wait,
            // so a rotation queued behind a predecessor stamps the epoch
            // it actually mints at rather than the one it was triggered
            // at. A read failure yields `null`, which fails the mint
            // closed rather than stamping a guess.
            // G-37 — the provider lock (not a per-group lock): this runs INSIDE
            // `GroupPushRouting`'s routing lock, and the discipline that keeps
            // the two deadlock-free is that push work only ever nests
            // routing -> provider, never the reverse (every repository-side
            // push call is LAUNCHED onto `pushScope`). Taking a per-group lock
            // here would add a routing -> group edge and make the routing
            // lock's bounded hold wait on a 15 s send publish.
            groupEpochFn = {
                providerLock.withLock { runCatching { group.epoch() }.getOrNull() }
            },
        )

    /**
     * ADR-0042 — post a control application message (the push-routing-secret
     * distribution envelope) to [group]'s journal under [providerLock], reusing
     * the audited persist-before-publish, fail-closed send primitive
     * ([ChatClient.sendMessage]): the sender ratchet advance is durably
     * persisted before the ciphertext reaches the DS, and the DS id is marked
     * own so the broadcaster's own poll drops the echo. Fires no push wake and
     * appends no timeline row (the repository does neither here).
     */
    private suspend fun broadcastControlToGroup(
        identity: Identity,
        groupIdHex: String,
        group: Group,
        envelope: ByteArray,
    ) {
        // ADR-0045 — the SEAL and the PUBLISH are both inside the per-group
        // lock, exactly as `sendMessage` / `sendTyping` / `sendReadReceipt`
        // already are. This is a CORRECTION of the previous "provider lock only"
        // shape, which left this the one send path on Android that could
        // STRADDLE an epoch change:
        //
        //   `lakko-mls` never sets `max_past_epochs`, so openmls's default of 0
        //   applies and a recipient keeps NO past-epoch secrets. A control
        //   envelope sealed at epoch N and published on the far side of a commit
        //   is therefore not late — it is permanently undecryptable for every
        //   recipient, dropped silently. Sealed AHEAD (a commit region
        //   eager-merged N -> N+1 and is parked in its publish, so this seals at
        //   N+1 and can reach the DS first) or BEHIND (an ADR-0045 sweep has
        //   STAGED at N and published its commit, and `create_message` gates on
        //   `is_active()` not `is_operational()`, so this still seals at N and
        //   necessarily lands after the commit — a CERTAINTY for the whole
        //   verdict window, not a race).
        //
        // What the envelope carries decides only what a loss COSTS, never
        // whether the hazard exists: a lost routing secret leaves a member
        // deferring until the next re-broadcast, and a lost beacon is absorbed
        // by ADR-0045's freshness window (sized for exactly one).
        //
        // ## Lock ordering — this ADDS a routing -> group edge
        //
        // The previous comment declined the group lock to avoid exactly that
        // edge. The edge is safe because the reverse never occurs: EVERY
        // repository-side push-routing call is `pushScope.launch`ed rather than
        // awaited under a group lock (`ensurePushRoutingBestEffort`,
        // `receivePushRoutingSecretBestEffort`, `rotatePushRoutingIfShrankBestEffort`,
        // `redistributePushRoutingBestEffort`, and `publishRemoveThenRotate`'s
        // own `scope.launch`), so no group -> routing edge exists and the global
        // order is routing -> group -> provider, consistent with group ->
        // provider everywhere else. It must stay that way: awaiting a
        // `GroupPushRouting` entry point under a group lock would close the
        // cycle.
        //
        // The cost the old comment named is real and is accepted rather than
        // denied: the routing lock's hold can now wait on a group lock held by a
        // send publish or by a sweep's two DS round-trips. Every `ports.broadcast`
        // call in `GroupPushRouting` runs under `bounded(...)`, so that wait is
        // capped and a timeout leaves the row PENDING for the existing
        // re-broadcast branch — the same degradation a DS error already produces.
        runGroupSend(
            lock = groupLock(groupIdHex),
            providerLock = providerLock,
            seal = {
                chatClient.prepareSend(
                    identity = identity,
                    group = group,
                    plaintext = envelope,
                    persistRatchet = { persistSnapshotDurableOrThrow() },
                )
            },
            publish = { prepared -> chatClient.publishPreparedSend(prepared) },
        )
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
                    ports = pushRoutingPorts(groupIdHex, group, identity),
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
     * called on the [providerLock] critical path (the `Group` handle is not safe
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
     * [providerLock] critical path.
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
     * poll loop's [providerLock] critical section (via `consumeEvents`), the
     * same discipline as the `group.groupId()` read below and as
     * [ensurePushRoutingBestEffort]'s `ownRole` read. Only the store read and
     * the network subscribe are deferred to [pushScope].
     */
    private fun receivePushRoutingSecretBestEffort(
        group: Group,
        identity: Identity,
        secret: ByteArray,
        mintEpoch: ULong,
        senderLeaf: UInt?,
    ) {
        // The lock key, alongside the group: `receiveGated` serializes its
        // whole read-decide-write per `(account, group)`. Without it two
        // envelopes from ONE poll batch — this sink fires per event, and
        // each launch lands on the multi-threaded [pushScope] — both decide
        // against the same pre-batch row and the survivor is chosen by
        // scheduling rather than by the ADR-0044 total order.
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val groupIdHex = try {
            DsClient.pseudonymHex(group.groupId())
        } catch (_: Throwable) {
            return
        }
        val owner = roleTableOwner(group)
        val senderKey = signatureKeyForLeaf(group, senderLeaf)
        // ADR-0044 — this device's OWN current epoch: the clamp ceiling the
        // gate bounds the incoming stamp against. Read SYNCHRONOUSLY here,
        // alongside the role table and the roster, because this runs inside
        // the poll loop's [providerLock] critical section. `null` (a torn
        // read) fails the gate CLOSED — with no ceiling a stamp cannot be
        // bounded at all.
        val ownGroupEpoch = try {
            group.epoch()
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "push routing: epoch read failed", cause)
            null
        }
        pushScope.launch {
            try {
                val verdict = GroupPushRouting.receiveGated(
                    accountIdB64 = accountIdB64,
                    groupIdHex = groupIdHex,
                    secret = secret,
                    mintEpoch = mintEpoch,
                    senderSignatureKey = senderKey,
                    owner = owner,
                    ownGroupEpoch = ownGroupEpoch,
                    ports = pushRoutingPorts(groupIdHex, group, identity),
                )
                if (!verdict.accepts) {
                    // ADR-0044 — the verdict is logged by NAME. The three
                    // refusal classes are operationally distinct: STALE is
                    // the F-A signature (somebody re-emitting a superseded
                    // secret), FUTURE is an out-of-range stamp, and the
                    // others are rank / verifiability failures. Both epochs
                    // are non-secret and a stale replay is invisible in the
                    // field without them (ADR-0044 open question).
                    android.util.Log.w(
                        "ChatRepository",
                        "push routing: REFUSED a secret ($verdict) " +
                            "(group=$groupIdHex, leaf=$senderLeaf, " +
                            "mintEpoch=$mintEpoch, ownEpoch=$ownGroupEpoch)",
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
     * ADR-0043 — stable per-member identities for the membership diff: the
     * lowercase-hex Ed25519 signature public keys from `group.members()`.
     *
     * Keys, NOT leaf indices: OpenMLS reuses a vacated leaf for the next
     * Add, so a remove-then-add pair inside ONE poll batch leaves both the
     * member COUNT and the leaf-index SET identical while the membership
     * genuinely changed. The signature key is stable for a member's lifetime
     * in the group.
     *
     * Returns `null` when the snapshot cannot be read (a torn group), which
     * [GroupPushRouting.membershipRotationDecision] treats as "unknown",
     * never as "everybody left". Must be called on the [providerLock] critical
     * path (the `Group` handle is not safe to touch off it).
     */
    private fun memberSignatureKeysOrNull(group: Group): List<String>? =
        try {
            group.members().map { DsClient.pseudonymHex(it.signaturePublicKey) }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "push routing: roster snapshot failed", cause)
            null
        }

    /**
     * ADR-0043 — this device's ADR-0022 role in [group], mapped onto the
     * FFI-free [GroupPushRouting.RotationRole] the pure decision core takes.
     * A read failure folds to [GroupPushRouting.RotationRole.NONE], which
     * never rotates (fail-closed).
     *
     * Read SYNCHRONOUSLY by the caller (never on [pushScope]), the same
     * discipline [ensurePushRoutingBestEffort]'s `ownRole` read follows: on
     * the poll path that lands inside the [providerLock] critical section; on
     * the own-remove path it runs immediately after the lock is released,
     * exactly where [redistributePushRoutingBestEffort] reads its own group
     * record after an add.
     */
    private fun rotationRole(group: Group, identity: Identity): GroupPushRouting.RotationRole =
        try {
            when (group.ownRole(identity)) {
                Role.OWNER -> GroupPushRouting.RotationRole.OWNER
                Role.ADMIN -> GroupPushRouting.RotationRole.ADMIN
                Role.MEMBER -> GroupPushRouting.RotationRole.MEMBER
                null -> GroupPushRouting.RotationRole.NONE
            }
        } catch (_: Throwable) {
            GroupPushRouting.RotationRole.NONE
        }

    /**
     * ADR-0043 / ADR-0044 (C-61) — the rotation STEP for [groupIdHex], as
     * a suspending closure the caller hands to
     * [GroupPushRouting.publishRemoveThenRotate], or `null` when there is
     * no session / live group record to rotate for.
     *
     * Deliberately NOT a fire-and-forget helper that launches on its own.
     * C-61 requires the rotation to be STARTED only after the Remove
     * commit has been published, and under ADR-0044 that ordering carries
     * two guarantees rather than one: the envelope's forward secrecy AND
     * the epoch it is STAMPED with (the mint reads `Group.epoch()`, so a
     * pre-commit rotation stamps the PRE-removal epoch, ties with the
     * secret it is replacing, and may lose the byte tiebreak). Returning a
     * closure instead of launching is what lets that ordering live in one
     * seam a test can drive, instead of in two adjacent statements anyone
     * can reorder.
     *
     * The ADR-0022 role is read INSIDE the closure — i.e. after the
     * removal has landed — under [providerLock], the same discipline
     * `broadcastControlToGroup` follows. Owner-or-admin gated inside
     * [GroupPushRouting.rotate], which also no-ops when this device holds
     * no secret, so rotation never becomes a second mint path.
     */
    private fun rotationStepOrNull(
        groupIdHex: String,
    ): (suspend () -> GroupPushRouting.RotationOutcome)? {
        val identity = canonicalIdentity.get() ?: return null
        val record = groups[groupIdHex] ?: return null
        val accountIdB64 = currentAccountIdB64.get() ?: return null
        return {
            // G-37 — provider lock only (a pure role-table read), released
            // before `rotate` takes the routing lock, so no routing -> group
            // edge is created. Same reasoning as `groupEpochFn`.
            val role = providerLock.withLock { rotationRole(record.group, identity) }
            GroupPushRouting.rotate(
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                ownRole = role,
                ports = pushRoutingPorts(groupIdHex, record.group, identity),
            )
        }
    }

    /**
     * ADR-0043 — the WIRE-OBSERVED rotation trigger: someone ELSE committed
     * a Remove and this poll batch applied it, so the membership SHRANK.
     *
     * A Commit surfaces from the poll loop carrying no membership delta
     * (surfacing removed leaves from `lakko-mls` would be exact but is an
     * invariant-#3 change to the MLS state machine and needs its own ADR),
     * so the removal is INFERRED from the `members()` snapshot taken either
     * side of the batch. This device's OWN removals never come through here
     * — they were applied locally before the poll, so the baseline is
     * already post-removal and shows no shrink; those rotate directly via
     * [rotationStepOrNull] behind [GroupPushRouting.publishRemoveThenRotate].
     *
     * Fire-and-forget; never breaks the poll loop.
     */
    private fun rotatePushRoutingIfShrankBestEffort(
        groupIdHex: String,
        group: Group,
        identity: Identity,
        previousMemberKeys: List<String>?,
        currentMemberKeys: List<String>?,
        /** ADR-0045 UX §6.2.8 item 3 — captured by the CALLER, synchronously,
         *  beside the post-batch roster snapshot, so the eviction and the
         *  shrink it caused are judged on the same commit. Passed rather than
         *  read here because this body runs on [pushScope], by which time
         *  another batch may have landed. */
        selfEvicted: Boolean,
    ) {
        val role = rotationRole(group, identity)
        pushScope.launch {
            try {
                GroupPushRouting.rotateIfMembershipShrank(
                    accountIdB64 = currentAccountIdB64.get() ?: return@launch,
                    groupIdHex = groupIdHex,
                    ownRole = role,
                    previousMemberKeys = previousMemberKeys,
                    currentMemberKeys = currentMemberKeys,
                    ports = pushRoutingPorts(groupIdHex, group, identity),
                    selfEvicted = selfEvicted,
                )
            } catch (_: Throwable) {
                // best-effort — push is a UX nicety
            }
        }
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
        val accountIdB64 = currentAccountIdB64.get() ?: return
        pushScope.launch {
            try {
                GroupPushRouting.redistribute(
                    accountIdB64,
                    groupIdHex,
                    pushRoutingPorts(groupIdHex, record.group, identity),
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

        // ADR-0045 UX §6.2.8 item 1 — the poll must STOP for a group this
        // device is no longer in, and stopping is not an optimisation.
        // `unprotect_message` refuses on `!is_active()` before it looks at the
        // message at all, and the refusal is NOT epoch-scoped, so every fetched
        // row would raise `UseAfterEviction`, be swallowed by the per-message
        // catch in [ChatClient.applyFetchedMessages], and advance the cursor —
        // one warning per row, forever, for a group that will never produce a
        // readable byte again. A loop that logs a refusal per row per poll is
        // indistinguishable from a bug, and this platform is the most exposed
        // of the three: it LONG-polls, so the loop is continuous rather than
        // event-driven.
        //
        // A GATE rather than a disposal, because `pollGroup` is re-entered from
        // the chat screen's long-poll loop, the Refresh button, the chat list's
        // `pollAllGroups` and every membership action, and there is no single
        // job to cancel. It also gates the tail sweep below, which an evicted
        // device has no business running either.
        //
        // The cursor is deliberately left where the last applied row put it
        // (§6.2.13): it costs nothing, and whether a rejoin wants it reset is
        // the rejoin path's question, not this one's.
        if (readIsActive { record.group.isActive() } == false) return

        // G-37 — the DS long poll (30 s on the live chat screen) used to be
        // awaited INSIDE the single repository-wide mutex. An open chat screen
        // therefore held that lock near-continuously and every send / edit /
        // reaction / membership commit on every group queued behind it.
        //
        // A timeout would be the wrong fix here — it would sever a long poll that
        // is behaving exactly as designed. Instead the network fetch (which
        // touches no MLS state) runs OUTSIDE both locks and only the apply +
        // cursor advance run inside them, guarded by a per-group in-flight latch
        // and a cursor-moved check. See [runSplitPoll] for the full ordering.
        //
        // The per-group lock guards this group's cursor + timeline record
        // (against a concurrent send on the same group); the provider lock
        // guards the shared OpenMLS provider (against every OTHER group's MLS
        // work). Deliberately NOT held across the fetch, so a 30 s long poll
        // still blocks neither.
        runSplitPoll(
            inFlight = record.pollInFlight,
            lock = groupLock(groupIdHex),
            providerLock = providerLock,
            readCursor = { record.cursor },
            fetch = { after ->
                chatClient.fetchGroupJournal(
                    // The record caches the group id, so the network half does
                    // not read the `Group` handle at all.
                    groupId = record.groupId,
                    after = after,
                    waitSecs = waitSecs,
                )
            },
        ) { after, raw ->
            // ADR-0043 — baseline for the membership diff, captured BEFORE
            // this batch applies any commit. A Commit carries no membership
            // delta out of the poll loop, so a removal committed by SOMEONE
            // ELSE is inferred from this snapshot against the post-poll one.
            // This device's OWN removals never show up here (they were
            // applied locally before this poll, so the baseline is already
            // post-removal); those rotate directly in
            // [removeMemberFromGroup].
            //
            // G-37 — still captured INSIDE the lock, immediately before the
            // apply. Hoisting it out to where the fetch now runs would race the
            // shrink diff against any commit landing in the fetch window.
            val membersBeforePoll = memberSignatureKeysOrNull(record.group)
            val result = chatClient.applyFetchedMessages(
                identity = identity,
                group = record.group,
                raw = raw,
                after = after,
            )
            record.consumeEvents(
                events = result.events,
                callSignalingSink = { payload ->
                    callSignalingSink?.invoke(groupIdHex, payload)
                },
                membershipProposalSink = { proposal, createdAt ->
                    // ADR-0045 UX §4 / §10 — stamp a PEER's request with the DS
                    // row's own timestamp. Without this the observation store
                    // held only our own requests, so `requestState` read
                    // `observedAtMs == null` for everyone else's and reported
                    // WAITING immediately — the quiet window never applied to
                    // the requests an admin actually sees.
                    recordReceivedProposalBestEffort(groupIdHex, proposal, createdAt)
                },
                committerBeaconSink = { senderLeaf, createdAt ->
                    // ADR-0045 S4b — inside the poll's provider critical
                    // section, which is where the `Group` handle is safe to
                    // touch and where the roster is the one this beacon was
                    // sealed against.
                    recordCommitterBeaconBestEffort(
                        group = record.group,
                        groupIdHex = groupIdHex,
                        senderLeaf = senderLeaf,
                        createdAt = createdAt,
                    )
                },
                pushRoutingSecretSink = { secret, mintEpoch, senderLeaf ->
                    // ADR-0042 — gate on the MLS-authenticated sender leaf
                    // (only the ADR-0022 owner may CHANGE the secret), then
                    // persist + subscribe under the derived opaque `push_gid`.
                    // ADR-0044 — and only if the envelope is strictly NEWER
                    // than what this device holds, and no further ahead than
                    // this device's own epoch.
                    // Fire-and-forget on the push scope so the poll loop is
                    // never blocked on the network subscribe; best-effort.
                    receivePushRoutingSecretBestEffort(
                        record.group,
                        identity,
                        secret,
                        mintEpoch,
                        senderLeaf,
                    )
                },
            )
            // THE ORDERING RULE — persist first, advance the cursor second, and
            // hold the cursor when the write does not land. See [DurableCursorGate]
            // for why the cursor may lag the durable MLS state but must never lead
            // it: since ADR-0045 a proposal applied here but not persisted, with
            // the cursor already past it, makes the commit that carries it by
            // reference permanently unappliable and strands this device at epoch N
            // forever.
            //
            // The persist is the FAIL-CLOSED [persistSnapshotDurableOrThrow], not
            // the swallowing [persistSnapshotBestEffort] this used to call. Both
            // run inside the provider lock this block already holds.
            applyDurableCursorBatch(
                gate = cursorGate,
                touchedMlsState = result.touchedMlsState,
                persist = { persistSnapshotDurableOrThrow() },
                advance = { record.advanceCursor(result.cursor) },
                onHeld = { cause ->
                    android.util.Log.w(
                        "ChatRepository",
                        "MLS snapshot persist failed — holding the journal cursor at " +
                            "${record.cursor} so this batch is re-fetched rather than lost",
                        cause,
                    )
                },
            )
            // The plaintext timeline is written either way — held or advanced —
            // and deliberately AFTER the gate so it records whatever cursor the
            // gate left. It is not MLS state: a stale timeline costs a re-render,
            // never a strand. Skipped for a batch that produced nothing, as
            // before, so an idle long poll still writes no file.
            if (result.events.isNotEmpty()) persistMessageLogBestEffort()
            // ADR-0043 — did someone else's Remove commit just land? Then the
            // evicted member still holds this group's push_routing_secret and
            // push-gw is still waking it. Re-key so nobody ever notifies the
            // id it can derive again. Driven off an actual before/after
            // member-SET comparison (signature keys, so a remove+add in one
            // batch is still seen — the count and the leaf indices would BOTH
            // be unchanged), never off a bare "a poll happened" tick.
            //
            // Owner-or-admin only, a no-op on growth, on an unchanged roster,
            // and on a torn snapshot. Fire-and-forget.
            //
            // ADR-0045 UX §6.2.8 item 3 — and this is the site the finding was
            // about. `selfEvicted` is snapshotted in the SAME synchronous
            // region as the post-batch roster, so the eviction and the shrink
            // it caused are judged on the same commit. Without it
            // `membershipRotationDecision` sees a member depart (us), reads
            // `ownRole` from the UNTOUCHED role table — a `Remove` rewrites no
            // `GroupContext` — and returns ROTATE, after which the device mints
            // a secret nobody will ever be notified under and the broadcast
            // throws.
            rotatePushRoutingIfShrankBestEffort(
                groupIdHex = groupIdHex,
                group = record.group,
                identity = identity,
                previousMemberKeys = membersBeforePoll,
                currentMemberKeys = memberSignatureKeysOrNull(record.group),
                selfEvicted = readIsActive { record.group.isActive() } == false,
            )
            publishThreadList()
        }
        // ADR-0045 — the committer sweep runs at the TAIL of every poll, AFTER
        // [runSplitPoll] has released the per-group lock: the sweep takes that
        // same lock itself and holds it across its own two DS round-trips, and
        // `Mutex` is not reentrant. Running it here rather than inside is also
        // what makes the stand-down re-read meaningful — the batch this poll
        // just applied is already consumed, so a non-empty re-read genuinely
        // means new rows.
        //
        // Short-circuits synchronously for a plain member, for a group with no
        // role table, and for a candidate with nothing pending, so the common
        // case costs one short provider region.
        sweepIfCommitter(groupIdHex)
    }

    /**
     * Pull new messages for every active group. Best-effort: per-group failures
     * are logged-and-swallowed so one bad apple doesn't block the others.
     *
     * G-37 — CONCURRENT, bounded by [POLL_ALL_CONCURRENCY]. This used to iterate
     * serially, so with a 30 s long poll the worst-case latency to see a new
     * message in ANY group was `waitSecs * N`. Now the N long polls overlap and
     * the bound is `waitSecs * ceil(N / POLL_ALL_CONCURRENCY)`. This only became
     * safe once the DS fetch moved outside the locks and the locks became
     * per-group — concurrent polls of DIFFERENT groups now contend only for the
     * short provider critical section of their applies, and the per-group
     * in-flight latch keeps a chat-screen poll and a list poll of the SAME group
     * from overlapping (the loser returns immediately without fetching).
     *
     * `supervisorScope` + a per-leg `try/catch` is what preserves the
     * best-effort contract: one group's failure must not cancel its siblings.
     * A plain `coroutineScope` would propagate the first failure and cancel the
     * rest, which is strictly worse than the serial loop it replaces.
     *
     * @param waitSecs same semantics as [pollGroup]; passed through to each
     *   per-group call.
     */
    public suspend fun pollAllGroups(waitSecs: Int = 0) {
        canonicalIdentity.get() ?: return
        // Snapshot the keys under the map's own monitor — `groups` is a
        // `synchronizedMap`, whose iterators are NOT self-synchronising.
        val groupIds = synchronized(groups) { groups.keys.toList() }
        // ADR-0045 UX §6.2.8 item 1 — filtered HERE as well as gated inside
        // [pollGroup], and the reason is a spin rather than a duplicate check:
        // the chat-list screen drives this in `while (isActive) { … }` and
        // relies on the long poll for its cadence, so a fleet of groups this
        // device has all been removed from would return instantly, forever.
        // Filtering first lets the empty case take one honest long-poll-shaped
        // pause instead.
        val active = groupIds.filter { isGroupActive(it) != false }
        if (active.isEmpty()) {
            if (waitSecs > 0) delay(waitSecs.toLong() * MILLIS_PER_SECOND)
            return
        }
        runPollAllGroups(
            groupIds = active,
            concurrency = POLL_ALL_CONCURRENCY,
        ) { groupIdHex ->
            pollGroup(groupIdHex, waitSecs = waitSecs)
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
            // ADR-0045 — drop the committer-beacon observations and the
            // pending-request observations. Neither is key material, but a
            // stale observation would make a later session defer to a candidate
            // it never actually heard from, and a stale request observation
            // would age a request that no longer exists. Same per-account reset
            // discipline as the outbox above.
            try {
                beaconObservations.clearForAccount(accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
            try {
                SharedPrefsPendingRequestStore(applicationContext).clearForAccount(accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
            // ADR-0045 S7 residual 1 — the `Not applied` rows follow the same
            // hygiene: they name a signature key from a session being discarded.
            try {
                SharedPrefsUnappliedRequestStore(applicationContext)
                    .clearForAccount(accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
            // ADR-0045 §6.1.10 — and the last-observed-owner memory. A stale
            // entry would make the NEXT account on this device read a role-table
            // reading it never took as a change, and announce a promotion it did
            // not witness — the one thing the §6.1.9 fail-open rule exists to
            // prevent.
            try {
                lastObservedOwnerStore.clearForAccount(accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
            // ADR-0045 UX §6.2.3 — and the durable `leaveRequestedAt` records.
            // They follow the same per-account reset discipline as the stores
            // above; they are never pruned with the pending sweep (they have to
            // outlive it, by construction), so this and `Delete this chat` are
            // the only two things that discard them.
            try {
                leaveRecords.clearForAccount(accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
            lastBeaconAtMs.clear()
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
        // G-37 — drop the per-group mutexes alongside the records they guard,
        // so an account switch does not carry a previous session's lock map.
        // Safe here: `wipeInMemoryState` runs on the bootstrap / logout path,
        // where there is no in-flight chat operation to strand — and a caller
        // that somehow still held one would simply hold the last reference to
        // an orphaned Mutex, never a lock a NEW operation could also acquire.
        groupLocks.clear()
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
    /**
     * The receive path's cursor-may-lag-never-lead gate.
     *
     * ONE per repository, which is the accurate granularity: `snapshotStorage()`
     * serialises the WHOLE provider, so a failed persist leaves every group
     * behind and any successful one flushes them all. See [DurableCursorGate].
     *
     * Every read/write of it happens inside [providerLock], the same region the
     * MLS mutations and the snapshot write run in, so it needs no lock of its own.
     */
    private val cursorGate = DurableCursorGate()

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
    private fun ackCommitBestEffort(id: String, commitMessageId: Long?, secondMessageId: Long?) {
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
     * Runs under [providerLock] (every commit call site holds it), so the provider
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
    // -----------------------------------------------------------------
    // ADR-0045 — proposal-based membership: the committer sweep + the
    // liveness beacon.
    // -----------------------------------------------------------------

    /**
     * ADR-0045 — does this group route membership through proposals?
     *
     * A group with **no role table** keeps direct commits, unchanged: it has no
     * owner, therefore no rank 0, therefore no committer, and every peer's
     * receipt gate drops proposals in such a group — so the propose path would
     * silently discard everything. ADR-0045 says this explicitly, and
     * `Group::create` has seeded a role table since ADR-0022, so no freshly
     * created group reaches it.
     *
     * This is NOT a dual path in ADR-0045's forbidden sense. The ADR rules out a
     * "legacy direct-commit fallback" — a second, ungated route into a group
     * that *has* moved to proposals. This is the disjoint case: a group with no
     * role table has no gate to bypass, because there is no table to read a role
     * from.
     *
     * A torn read is treated as "no role table", which keeps the direct path
     * working rather than routing into a proposal path whose gate cannot
     * resolve.
     *
     * MUST be called on the [providerLock] critical path.
     */
    private fun usesProposalPath(group: Group): Boolean = try {
        group.groupRoles() != null
    } catch (_: Throwable) {
        false
    }

    /**
     * ADR-0045 S4b — the ONE place the FFI's `ActingCommitter` and the
     * client-side [ActingCommitter] meet. Declared as a conversion rather than
     * reusing the FFI type throughout so `CommitterSweep.kt` and
     * `CommitterBeacon.kt` stay drivable from a plain JVM host test.
     */
    private fun toActingCommitter(
        verdict: uniffi.lakko_ffi_uniffi.ActingCommitter,
    ): ActingCommitter = when (verdict) {
        is uniffi.lakko_ffi_uniffi.ActingCommitter.Act -> ActingCommitter.Act
        is uniffi.lakko_ffi_uniffi.ActingCommitter.Defer -> ActingCommitter.Defer(verdict.toRank)
        is uniffi.lakko_ffi_uniffi.ActingCommitter.NotACandidate -> ActingCommitter.NotACandidate
    }

    /**
     * ADR-0045 — the pending-request observation store. Plain
     * `SharedPreferences`: a request key names a proposal already retained in
     * the core's own pending set, plus a timestamp.
     */
    private val pendingRequestObservations: PendingRequestStore by lazy {
        SharedPrefsPendingRequestStore(applicationContext)
    }

    /**
     * ADR-0045 S7 residual 1 — the `Not applied` rows. Deliberately NOT part of
     * [pendingRequestObservations]: that store is pruned against the core's live
     * pending set on every fold, and these rows must OUTLIVE the sweep that
     * empties it — a row is created at the exact moment its observation is
     * pruned.
     */
    private val unappliedRequests: UnappliedRequestStore by lazy {
        SharedPrefsUnappliedRequestStore(applicationContext)
    }

    /**
     * ADR-0045 — how many authorised proposals are waiting for a sweep in
     * [groupIdHex]. `0` on any failure, which is the direction that says
     * nothing rather than the direction that invents a queue.
     */
    public fun pendingProposalCount(groupIdHex: String): Int {
        val record = groups[groupIdHex] ?: return 0
        return try {
            record.group.pendingProposals().size
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "pendingProposalCount failed", cause)
            0
        }
    }

    /**
     * ADR-0045 S7 residual 1 — the `Not applied` rows the user has not
     * dismissed. Read after [pendingMembershipRequests], which is what detects
     * and records them.
     */
    public fun unappliedAdds(groupIdHex: String): List<UnappliedAdd> {
        val accountIdB64 = currentAccountIdB64.get() ?: return emptyList()
        return try {
            loadUnapplied(unappliedRequests, accountIdB64, groupIdHex)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "unappliedAdds failed", cause)
            emptyList()
        }
    }

    /** ADR-0045 S7 residual 1 — dismiss one `Not applied` row. It stays
     *  dismissed: the detection re-runs on every fold, so without the tombstone
     *  the notice could not be dismissed at all. */
    public fun dismissUnappliedAdd(groupIdHex: String, addSignatureKeyB64: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            dismissUnapplied(unappliedRequests, accountIdB64, groupIdHex, addSignatureKeyB64)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "dismissUnappliedAdd failed", cause)
        }
    }

    /**
     * ADR-0045 — record that a request THIS device just published landed, so the
     * quiet window can be measured from now and so the panel knows it is ours
     * (the UX §10 own-request visibility exception, without which a self-remove
     * is an action with no feedback).
     *
     * The stamp is this device's own clock, unlike a RECEIVED request, whose
     * stamp must be the DS row's. That asymmetry is deliberate and harmless:
     * the DS-row rule exists so every member measures the SAME request from the
     * same instant, and for a request this device authored there is no other
     * member's reading to agree with yet — the row's own timestamp arrives on
     * the next poll and, because the FIRST observation of a key wins, does not
     * overwrite this one.
     *
     * [transferToB64] (ADR-0045 §6.1.10) is supplied only by
     * [transferGroupOwnership]: a role change carries no payload, so the
     * successor's key on our own record is the only thing that later tells a
     * transfer from a promote, and it is what names the person in
     * `Waiting to make {name} the owner.`
     */
    private fun noteOwnRequestBestEffort(
        groupIdHex: String,
        key: String,
        transferToB64: String? = null,
        joinRequest: Boolean = false,
    ) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            recordPendingRequestObservation(
                store = pendingRequestObservations,
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                key = key,
                atMs = System.currentTimeMillis(),
                own = true,
                transferToB64 = transferToB64,
                joinRequest = joinRequest,
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "pending request: note own failed", cause)
        }
    }

    /**
     * ADR-0045 UX §4 — stamp a proposal a PEER published, with the DS row's own
     * timestamp.
     *
     * The counterpart to [noteOwnRequestBestEffort], and the asymmetry between
     * the two is the point: an own request is stamped from the local clock
     * because there is no other member's reading to agree with yet, while a
     * received one MUST carry the DS row's server-assigned time, so every
     * member measures the same request from the same instant. Because the first
     * observation of a key wins, the two never fight — our own request's echo
     * arriving off the journal cannot overwrite the local stamp, and cannot
     * clear the `own` flag §10's visibility exception rests on.
     *
     * Best-effort throughout: a store failure costs the quiet window, never the
     * request, which lives in the core's pending set either way.
     */
    private fun recordReceivedProposalBestEffort(
        groupIdHex: String,
        proposal: ProposalRecord,
        createdAt: String,
    ) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            recordReceivedProposalObservation(
                store = pendingRequestObservations,
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                record = proposal,
                // The SAME parser the typing fold uses, not a second one:
                // two hand-rolled DS-timestamp readers in one file is the
                // drift class that makes one surface silently disagree with
                // another about when a row landed.
                atMs = GroupRecord.parseIsoMillis(createdAt),
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "pending request: note peer failed", cause)
        }
    }

    /**
     * ADR-0045 §6.1.10 — the last-observed-owner store, the second of the two
     * local additions ownership transfer needs (the first, the successor key,
     * rides on the pending-request observation above).
     *
     * Plain `SharedPreferences`: an owner signature key is already public inside
     * the group. Same posture as [pendingRequestObservations] and
     * [beaconObservations].
     */
    private val lastObservedOwnerStore: LastObservedOwnerStore by lazy {
        SharedPrefsLastObservedOwnerStore(applicationContext)
    }

    // -----------------------------------------------------------------
    // ADR-0045 UX §6.2 — the end of your own membership
    //
    // Every rule and every string lives in `MembershipEnded.kt`, where the host
    // suite can run them; what is here is the wiring that feeds them live group
    // state and performs the teardown they decide.
    // -----------------------------------------------------------------

    /**
     * §6.2.3 / §9 — the durable `leaveRequestedAt` record.
     *
     * A separate store rather than a field on [pendingRequestObservations],
     * and the reason is that the failure would be silent: the observation for
     * a leave lives under `remove:{ownLeaf}` and
     * [prunePendingRequestObservations] drops it the moment the core stops
     * holding the request — which is exactly what the sweep that removes us
     * does. The poll batch carrying the eviction would therefore destroy the
     * only evidence that we asked for it.
     */
    private val leaveRecords: LeaveRecordStore by lazy {
        SharedPrefsLeaveRecordStore(applicationContext)
    }

    /**
     * §6.2.2 — is this device still a member of [groupIdHex]?
     *
     * `null` when there is no live record for the id or the handle could not be
     * read; [membershipState] treats that as still-a-member, because an
     * unreadable handle is not evidence of an eviction.
     *
     * Safe off [providerLock]: `is_active()` is a field read on the in-memory
     * group state that touches no provider storage, and the FFI `Group` guards
     * it with its own `Mutex`.
     */
    public fun isGroupActive(groupIdHex: String): Boolean? {
        val record = groups[groupIdHex] ?: return null
        return readIsActive { record.group.isActive() }
    }

    /**
     * §6.2.3 — the stamp at which this device published a self-targeted leave
     * for [groupIdHex], or `null`.
     *
     * `null` is the FAIL-SAFE case and the direction is the point: the fallback
     * copy is the *removed* one, which is true of someone who left as well, so
     * a device that lost the record (the ADR-0040 crash window, a reinstall, a
     * restore) degrades to a sentence that is still correct rather than to one
     * that is wrong.
     */
    public fun leaveRequestedAt(groupIdHex: String): Long? {
        val accountIdB64 = currentAccountIdB64.get() ?: return null
        return loadLeaveRequestedAt(leaveRecords, accountIdB64, groupIdHex)
    }

    /**
     * §6.2.3 — record that this device published a self-targeted leave.
     *
     * Deliberately NOT folded into [noteOwnRequestBestEffort]: that store is
     * pruned against the core's live pending set on every fold, and this must
     * outlive the sweep that empties it. Written at PUBLISH, never pruned with
     * the pending sweep, and discarded only with the group record itself
     * ([deleteEndedChat]) or the account ([forgetDevice]). First stamp wins.
     */
    public fun noteOwnLeaveRequested(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        recordLeaveRequested(
            store = leaveRecords,
            accountIdB64 = accountIdB64,
            groupIdHex = groupIdHex,
            atMs = System.currentTimeMillis(),
        )
    }

    /**
     * §6.2.8 item 5 — retire THIS device's push subscription for a group it is
     * no longer in.
     *
     * ADR-0043 recorded the evicted member's push-gw row as never deleted
     * because *"nobody else can delete it and no client has 'you were removed'
     * handling"*. This is that handling: the endpoint deletes exactly the
     * `(device, group)` row named in the request and a client knows only its
     * OWN device pseudonym, so the ex-member is the only party that can end its
     * own wake stream. It closes the other half of G-36 residual 2 — rotation
     * stops the group notifying the old id; this stops the ex-member being
     * subscribed to anything at all.
     *
     * **The honest bound:** it needs this device to come online and run it, so
     * it COMPLEMENTS the group's rotation and replaces nothing. A phone that is
     * never opened again keeps its row, exactly as before.
     *
     * Returns `true` when the teardown is DONE — which includes holding no
     * secret at all, since then nothing was ever subscribed under a `push_gid`
     * for this group. `false` means "could not act yet", and the caller keeps
     * the step owed. [GroupPushRouting.retireOwnSubscription] takes the shared
     * per-`(account, group)` routing lock (it is the fifth entry point to it)
     * and never throws.
     */
    public suspend fun retirePushSubscriptionAfterEviction(groupIdHex: String): Boolean {
        val identity = canonicalIdentity.get() ?: return false
        val record = groups[groupIdHex] ?: return false
        val accountIdB64 = currentAccountIdB64.get() ?: return false
        // On IO, like every other push leg (`pushScope` is `Dispatchers.IO`
        // "because both legs are network-heavy"). AWAITED rather than launched
        // fire-and-forget, unlike its siblings, because the caller's teardown
        // latch releases the step on failure and cannot do that without the
        // answer — and the disk read + push-gw round-trip inside must not run
        // on the caller's Main-confined collector.
        return withContext(Dispatchers.IO) {
            GroupPushRouting.retireOwnSubscription(
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                ports = pushRoutingPorts(groupIdHex, record.group, identity),
            )
        }
    }

    /**
     * §6.2.4 — `Delete this chat`.
     *
     * Removes this group's transcript from the message log, its pending-request
     * observations, its durable leave record, its known-groups entry and its
     * in-memory record. The REGISTRY row goes LAST, so a failure above leaves
     * the chat reachable rather than orphaning a transcript behind a row the
     * user can no longer open.
     *
     * **What it does NOT remove, which is why [DELETE_CHAT_BODY] is exact
     * rather than sweeping:** the inactive MLS group stays in the provider
     * snapshot as inert bytes. `Group` exposes no delete, so a `Group::delete`
     * is a real (small) core follow-up rather than something this rides free.
     *
     * The push subscription is already gone by this point: the §6.2.8 teardown
     * retires it at the moment of the eviction, not at the moment of the
     * delete, so a user who never presses this button still stops being woken.
     */
    public fun deleteEndedChat(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            MessageLogStore.deleteGroupFromLog(applicationContext, accountIdB64, groupIdHex)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0045 delete chat: message log", cause)
        }
        try {
            prunePendingRequestObservations(
                store = pendingRequestObservations,
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                liveKeys = emptyList(),
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0045 delete chat: observations", cause)
        }
        try {
            unappliedRequests.clearForGroup(accountIdB64, groupIdHex)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0045 delete chat: unapplied rows", cause)
        }
        try {
            leaveRecords.clear(accountIdB64, groupIdHex)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0045 delete chat: leave record", cause)
        }
        try {
            knownGroupsStore.remove(accountIdB64, groupIdHex)
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0045 delete chat: known groups", cause)
        }
        // LAST, and only after everything above had its chance: dropping the
        // registry row is what makes the chat unreachable, so it must not
        // happen while a transcript is still behind it.
        val removed = synchronized(groups) { groups.remove(groupIdHex) }
        try {
            removed?.group?.close()
        } catch (_: Throwable) {
            // best-effort — releasing the Rust-side Arc must not fail a delete.
        }
        publishThreadList()
    }

    /**
     * The owner signature key (base64) this device last observed for
     * [groupIdHex], or `null` when it never observed one.
     *
     * `null` is the FAIL-OPEN case and must stay one: the §6.1.9 new-owner
     * notice is a client-side inference from a role-table CHANGE, and a device
     * that first observes the table already naming it owner has witnessed no
     * change to report.
     */
    internal fun lastObservedOwner(groupIdHex: String): String? {
        val accountIdB64 = currentAccountIdB64.get() ?: return null
        return lastObservedOwnerStore.load(accountIdB64, groupIdHex)
    }

    /** Record the owner this device is observing now. The caller decides the
     *  §6.1.9 notice FIRST, so the decision always compares against the
     *  PREVIOUS reading. */
    internal fun rememberObservedOwner(groupIdHex: String, ownerKeyB64: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        lastObservedOwnerStore.save(accountIdB64, groupIdHex, ownerKeyB64)
    }

    /**
     * ADR-0045 — the live pending membership requests for [groupIdHex], joined
     * from the core's retained set (the truth about WHAT is pending) and the
     * local observation store (WHEN each landed, and whether it is ours).
     *
     * Prunes the observation store against the live set in the same pass, so a
     * swept or superseded request leaves no residue. Empty on any failure — the
     * panel showing nothing is a strictly better failure than the panel showing
     * a request that is not there.
     */
    public fun pendingMembershipRequests(groupIdHex: String): List<PendingRequest> {
        val accountIdB64 = currentAccountIdB64.get() ?: return emptyList()
        val record = groups[groupIdHex] ?: return emptyList()
        return try {
            val records = record.group.pendingProposals()
            val observations = pendingRequestObservations.load(accountIdB64, groupIdHex)
            val folded = foldPendingRequests(
                records = records,
                observations = observations,
                nowMs = System.currentTimeMillis(),
            )
            // ADR-0045 S7 residual 1 — an own `add` that has left the pending
            // set without reaching the roster was not carried by whatever commit
            // emptied the set, and nothing will ever carry it. Detected HERE, in
            // the same pass that prunes, because the observation about to be
            // dropped is the only record that the request existed at all. The
            // roster is read in this same pass for the same reason: the merge
            // that empties the pending set is the merge that moves the roster.
            // A torn read is `null` (UNKNOWN), never an empty group.
            val rosterKeysB64 = try {
                record.group.members().map { outboxB64(it.signaturePublicKey) }
            } catch (cause: Throwable) {
                android.util.Log.w("ChatRepository", "roster read for unapplied failed", cause)
                null
            }
            recordUnapplied(
                store = unappliedRequests,
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                items = unappliedAddRequests(observations, folded.liveKeys, rosterKeysB64),
            )
            prunePendingRequestObservations(
                store = pendingRequestObservations,
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                liveKeys = folded.liveKeys,
            )
            folded.requests
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "pendingMembershipRequests failed", cause)
            emptyList()
        }
    }

    /**
     * ADR-0045 — the beacon observation store. Plain `SharedPreferences`: an
     * observation is a signature key already public inside the group plus a
     * server-assigned timestamp, so there is no key material to protect.
     */
    private val beaconObservations: BeaconObservationStore by lazy {
        SharedPrefsBeaconObservationStore(applicationContext)
    }

    /** ADR-0045 — when this device last emitted a beacon, per group, from its
     *  OWN local clock. Legitimate here and nowhere else in the beacon design,
     *  because it governs only this device's send cadence and feeds no judgement
     *  about anybody else. */
    private val lastBeaconAtMs = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /**
     * ADR-0045 S4b — record that a beacon landed on [groupIdHex].
     *
     * The MLS sender leaf is resolved to a signature KEY **here, at receipt**,
     * because that is the only moment the roster is authenticated in the epoch
     * the beacon was sealed at — and the KEY is what is persisted, never the
     * leaf: OpenMLS reuses a vacated leaf for the next member added, so a stored
     * leaf silently comes to name somebody else. The RANK is not derived here at
     * all; it is derived at verdict time, against the role table as it stands
     * then (see [actingCommitterNow]).
     *
     * The age comes from the DS row's server-assigned [createdAt], never a local
     * clock: every member reads the identical value for the identical row. A row
     * whose timestamp does not parse is DROPPED rather than stored — there is no
     * age to measure, so it is no evidence of liveness, and storing it as
     * "fresh" would defer this device forever with nothing to age out.
     *
     * Called from the poll's [providerLock] critical section (the `Group` handle
     * is not safe to touch off it). Best-effort throughout: losing an
     * observation costs a takeover's promptness, never its correctness.
     */
    private fun recordCommitterBeaconBestEffort(
        group: Group,
        groupIdHex: String,
        senderLeaf: UInt?,
        createdAt: String,
    ) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val senderKey = signatureKeyForLeaf(group, senderLeaf) ?: return
        val atMs = GroupRecord.parseIsoMillis(createdAt) ?: return
        try {
            recordBeaconObservation(
                store = beaconObservations,
                accountIdB64 = accountIdB64,
                groupIdHex = groupIdHex,
                senderKeyB64 = outboxB64(senderKey),
                atMs = atMs,
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "committer beacon: record failed", cause)
        }
    }

    /**
     * ADR-0045 S4b — the acting-committer verdict, taken fresh on every sweep
     * attempt.
     *
     * **Every rank in the projection is derived HERE and now**, by
     * `Group.committerRank` against the group's live `0xFF02` role table — never
     * carried over from when the beacon arrived. A rank is a position in that
     * table's canonical order, so promoting, demoting or removing any admin
     * renumbers every admin sorting after it; a rank captured under one ordering
     * and compared against this device's own current rank is not stale, it is
     * meaningless.
     *
     * Fails closed to `Defer` on anything it cannot read. Deferring costs a
     * sweep attempt; acting on a view of who else is running that this device
     * could not actually read is the hazard.
     *
     * Takes [providerLock] itself, so it must NOT be called from inside one.
     */
    private suspend fun actingCommitterNow(
        group: Group,
        groupIdHex: String,
        ownRank: UInt,
    ): ActingCommitter {
        val accountIdB64 = currentAccountIdB64.get()
            ?: return ActingCommitter.Defer(null)
        return try {
            providerLock.withLock {
                decideActingCommitter(
                    store = beaconObservations,
                    accountIdB64 = accountIdB64,
                    groupIdHex = groupIdHex,
                    ownRank = ownRank,
                    rankOf = { senderKeyB64 ->
                        try {
                            group.committerRank(outboxUnb64(senderKeyB64))
                        } catch (_: Throwable) {
                            null
                        }
                    },
                    nowMs = System.currentTimeMillis(),
                    // Read off the FFI, never hand-copied — the interval and the
                    // window are two halves of one budget and a local literal is
                    // the drift class ADR-0044 exists about.
                    freshnessWindowMs = uniffi.lakko_ffi_uniffi.committerBeaconFreshnessWindowMs(),
                    fold = { rank, observed, windowMs ->
                        toActingCommitter(
                            uniffi.lakko_ffi_uniffi.actingCommitterVerdict(
                                rank,
                                observed.map {
                                    uniffi.lakko_ffi_uniffi.BeaconObservation(it.rank, it.ageMs)
                                },
                                windowMs,
                            ),
                        )
                    },
                )
            }
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "committer beacon: verdict unreadable", cause)
            ActingCommitter.Defer(null)
        }
    }

    /**
     * ADR-0045 S4b — emit this device's own liveness beacon for [groupIdHex],
     * if it is due.
     *
     * **The foreground gate is the load-bearing part and it is a caller
     * obligation the core cannot check.** A beacon means *able to sweep*, and on
     * this client a sweep runs only at the tail of a poll driven by a RESUMED
     * chat screen; the ADR-0041 / C-59 background decrypt is read-only-throwaway
     * and structurally cannot commit, and a backgrounded process can be frozen
     * at any moment. A beacon from the background would therefore be a lie that
     * costs the group a takeover: every other candidate would defer to a device
     * that is not going to sweep, and would keep deferring for as long as the
     * backgrounded app kept beaconing.
     *
     * Everything else — only-candidates-emit, first-of-session-immediate, one
     * per interval — is the core's `shouldEmitBeacon`, called through the gate.
     *
     * Best-effort and silent: nobody is watching a beacon, and a failed emit
     * only means this device does not claim the duty on this tick.
     *
     * @return the verdict, so a caller (and a test) can see WHY nothing went out.
     */
    public suspend fun emitCommitterBeaconIfDue(
        groupIdHex: String,
        foreground: Boolean,
    ): String {
        // Short-circuited FIRST so a backgrounded app does no work at all — the
        // same answer [beaconEmitVerdict] gives below, reached before the
        // Ed25519 key derivation the rank read costs. The gate there stays the
        // authority.
        if (!foreground) return BeaconEmitVerdict.NOT_FOREGROUND.name
        val identity = canonicalIdentity.get() ?: return BeaconEmitVerdict.NOT_A_CANDIDATE.name
        val record = groups[groupIdHex] ?: return BeaconEmitVerdict.NOT_A_CANDIDATE.name
        val ownRank = try {
            providerLock.withLock {
                if (!usesProposalPath(record.group)) return BeaconEmitVerdict.NOT_A_CANDIDATE.name
                record.group.ownCommitterRank(identity)
            }
        } catch (_: Throwable) {
            // No identity / torn group — this device cannot establish that it is
            // a candidate, so it must not claim to be one.
            return BeaconEmitVerdict.NOT_A_CANDIDATE.name
        }
        val verdict = beaconEmitVerdict(
            ownRank = ownRank,
            foreground = foreground,
            lastEmittedAtMs = lastBeaconAtMs[groupIdHex],
            nowMs = System.currentTimeMillis(),
            throttle = { rank, last, now -> uniffi.lakko_ffi_uniffi.shouldEmitBeacon(rank, last, now) },
            // ADR-0045 UX §6.2.8 item 2 — an evicted device must never beacon.
            // The role table a `Remove` leaves behind still names an evicted
            // admin an admin, so `ownRank` above came back non-null and the
            // throttle would say EMIT; the gate is what stops a throwing emit
            // being retried on every tick. The ticker is stopped too (the
            // §6.2.8 one-shot teardown), so this is belt and braces — but it is
            // the half that is a GATE, and the gate is what makes a surviving
            // ticker harmless rather than merely unlikely.
            membershipEnded = readIsActive { record.group.isActive() } == false,
        )
        if (verdict != BeaconEmitVerdict.EMIT) return verdict.name
        // Stamped BEFORE the send, so a publish that is slow or that fails
        // cannot let the next tick fire a second beacon a few seconds later. It
        // is also this device's re-entrancy guard: a publish outlasting the tick
        // interval reads as throttled on the next pass rather than doubling up.
        lastBeaconAtMs[groupIdHex] = System.currentTimeMillis()
        try {
            broadcastControlToGroup(
                identity = identity,
                groupIdHex = groupIdHex,
                group = record.group,
                envelope = uniffi.lakko_ffi_uniffi.composeCommitterBeacon(),
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatRepository", "ADR-0045 committer beacon not emitted", cause)
        }
        return verdict.name
    }

    /**
     * ADR-0045 S4a/S4b — run one committer sweep for [groupIdHex], if this
     * device is the acting committer.
     *
     * The committer set is the ADR-0022 owner plus admins, ordered by
     * `ownCommitterRank()`; which of them ACTS is decided by the beacon fold.
     * Rank 0 — the owner — always acts, because no rank is lower; an admin acts
     * when nobody strictly better placed has beaconed inside the freshness
     * window, which includes the case where it has heard nothing at all. So an
     * admin-only group no longer waits for an absent owner.
     *
     * Short-circuits before touching the network when this device is not a
     * candidate, or when nothing is pending and no verdict is owed — so wiring
     * this into the tail of every poll costs one short provider region in the
     * common case.
     *
     * Runs under the same per-group lock the commit and poll regions take. The
     * whole region (stage -> publish -> verdict -> merge/abandon) is inside it,
     * network round-trips included, because the verdict is a read-modify-write
     * over both the group epoch and the journal cursor; a poll interleaving in
     * the middle would not merely slow it down, it would make the verdict wrong.
     * The blast radius is one group — G-37's concern was a repository-wide lock.
     */
    public suspend fun sweepIfCommitter(groupIdHex: String): SweepOutcome? {
        val identity = canonicalIdentity.get() ?: return null
        val record = groups[groupIdHex] ?: return null
        if (currentAccountIdB64.get() == null) return null
        // ADR-0043 — the baseline for the membership diff, captured INSIDE the
        // group lock immediately before the sweep, so a commit landing in the
        // window cannot race the shrink diff. Lock order is always group first,
        // provider second.
        var membersBefore: List<String>? = null
        val outcome = try {
            groupLock(groupIdHex).withLock {
                membersBefore = providerLock.withLock { memberSignatureKeysOrNull(record.group) }
                runCommitterSweep(sweepPortsFor(record, identity, groupIdHex))
            }
        } catch (cause: Throwable) {
            // A verdict this device could not read leaves the sweep staged for
            // the next attempt (which runs at the tail of every poll). Never
            // surfaced as a user-facing error: nothing the user did failed, and
            // the request is still live.
            android.util.Log.w("ChatRepository", "ADR-0045 sweep did not complete", cause)
            return null
        }
        if (outcome !is SweepOutcome.Committed) return outcome
        publishThreadList()
        // ADR-0043 / C-61 — the sweep just landed a membership change, and a
        // Remove among it means the evicted member still derives the current
        // push_gid. Started strictly AFTER the merge + publish, so the new
        // secret is sealed at the post-removal epoch. Fire-and-forget.
        //
        // ADR-0045 UX §6.2.8 item 3 — a sweep CAN carry our own removal: an
        // admin can sweep a `Leave` it proposed for itself, and an owner-rank
        // committer sweeps whatever is pending including a `Remove` naming it.
        // Captured beside the post-sweep roster for the same reason as on the
        // poll path.
        rotatePushRoutingIfShrankBestEffort(
            groupIdHex = groupIdHex,
            group = record.group,
            identity = identity,
            previousMemberKeys = membersBefore,
            currentMemberKeys = providerLock.withLock { memberSignatureKeysOrNull(record.group) },
            selfEvicted = readIsActive { record.group.isActive() } == false,
        )
        // ADR-0042 — a swept ADD needs the current routing secret, exactly as
        // the direct add path re-broadcasts it. No-op when this device holds
        // none.
        if (outcome.addedCount > 0) redistributePushRoutingBestEffort(groupIdHex)
        return outcome
    }

    /**
     * Adapt a live [GroupRecord] onto the [SweepPorts] the ADR-0045 sweep
     * orchestration runs over.
     *
     * The adapter is deliberately thin and does no deciding: every rule lives in
     * `CommitterSweep.kt`, where it is host-testable without the FFI, a
     * `Context` or a network.
     */
    private fun sweepPortsFor(
        record: GroupRecord,
        identity: Identity,
        groupIdHex: String,
    ): SweepPorts {
        val accountIdB64 = requireAccountId()
        val groupIdB64 = outboxB64(record.groupId)
        return SweepPorts(
            group = object : SweepGroupPort {
                override suspend fun <T> exclusiveProvider(block: suspend () -> T): T =
                    providerLock.withLock { block() }

                override fun epoch(): ULong = record.group.epoch()

                override fun ownCommitterRank(): UInt? = record.group.ownCommitterRank(identity)

                override fun pendingProposalCount(): Int = record.group.pendingProposals().size

                override fun hasStagedSweep(): Boolean = record.group.hasStagedSweep()

                override fun stagePendingProposals(): StagedSweepArtifacts? {
                    val staged = record.group.stagePendingProposals(identity) ?: return null
                    // `swept` reports what the commit REALLY carries, read back
                    // off the staged commit by the core rather than predicted by
                    // the caller — which matters here because it is what
                    // addresses the Welcome.
                    val added = staged.swept.mapNotNull {
                        (it.kind as? uniffi.lakko_ffi_uniffi.ProposalKind.Add)?.signatureKey
                    }
                    return StagedSweepArtifacts(
                        commit = staged.commit,
                        welcome = staged.welcome,
                        addedSignatureKeys = added,
                    )
                }

                override fun mergeStagedSweep() {
                    record.group.mergeStagedSweep(identity)
                }

                override fun abandonStagedSweep() {
                    record.group.abandonStagedSweep(identity)
                }

                // ADR-0045 — apply AND surface. The verdict consumes every row the
                // DS ordered below our own commit and the shared cursor then
                // advances past them, so a row applied here is a row the poll will
                // never fetch again: discarding what it decrypted loses an ordinary
                // chat message from the timeline permanently and silently. Routed
                // through the SAME [toEvent] + [GroupRecord.consumeEvents] the poll
                // uses, so every envelope kind is dispatched by one ladder.
                override fun applyIncoming(row: SweepJournalRow) {
                    // The MLS apply is what the verdict reads, so it is deliberately
                    // OUTSIDE the guard below: a throw here must reach `adjudicate`,
                    // which skips the row exactly as the poll loop does.
                    val processed =
                        record.group.processIncomingVerifyingRoles(identity, row.payload)
                    try {
                        record.consumeEvents(
                            events = listOf(
                                toEvent(
                                    messageId = row.id,
                                    createdAt = row.createdAt,
                                    processed = processed,
                                    // The SECOND receive path, and it must
                                    // resolve identically to the poll: the
                                    // roster is read HERE, one line after the
                                    // apply, because that is the epoch this row
                                    // decrypted at. This path consumes rows the
                                    // poll will never fetch again, so a leaf
                                    // resolved later here is unrecoverable.
                                    resolveSender = { leaf ->
                                        resolveMemberIdentity(rosterOf(record.group), leaf)
                                    },
                                ),
                            ),
                            callSignalingSink = { payload ->
                                callSignalingSink?.invoke(groupIdHex, payload)
                            },
                            membershipProposalSink = { proposal, createdAt ->
                                // The sweep's own apply pass consumes rows the
                                // poll will never fetch again, so a proposal
                                // that landed below our commit is stamped here
                                // or nowhere.
                                recordReceivedProposalBestEffort(
                                    groupIdHex, proposal, createdAt,
                                )
                            },
                            pushRoutingSecretSink = { secret, mintEpoch, senderLeaf ->
                                receivePushRoutingSecretBestEffort(
                                    record.group,
                                    identity,
                                    secret,
                                    mintEpoch,
                                    senderLeaf,
                                )
                            },
                            committerBeaconSink = { senderLeaf, createdAt ->
                                recordCommitterBeaconBestEffort(
                                    group = record.group,
                                    groupIdHex = groupIdHex,
                                    senderLeaf = senderLeaf,
                                    createdAt = createdAt,
                                )
                            },
                        )
                        // Per row, and deliberately not deferred to the end of the
                        // sweep: this is what pushes `record.messages` into the
                        // StateFlow the chat screen reads, and it must happen on a
                        // LOST verdict and on a thrown one too. Whether our own
                        // commit won the order says nothing about the rows below
                        // it — they were applied, the ratchet moved and the cursor
                        // advanced past them either way, so the only question left
                        // is whether the user gets to read them. Cheap because the
                        // window this loop consumes is narrow by construction.
                        publishThreadList()
                    } catch (cause: Throwable) {
                        // The ratchet has already moved and the row is consumed, so
                        // an event that could not be built costs one timeline row —
                        // never the verdict, which is why this is not allowed to
                        // look like a failed apply.
                        android.util.Log.w(
                            "ChatRepository",
                            "ADR-0045 swept row not surfaced: ${row.id}",
                            cause,
                        )
                    }
                }
            },
            journal = object : SweepJournalPort {
                override suspend fun fetch(after: Long): List<SweepJournalRow> =
                    chatClient.fetchGroupJournal(
                        groupId = record.groupId,
                        after = after,
                        waitSecs = 0,
                    ).messages.map {
                        // ADR-0045 — `createdAt` carried so an applied row can
                        // become a timeline event.
                        SweepJournalRow(it.id, it.payload, it.createdAt)
                    }

                override suspend fun publishGroup(bytes: ByteArray): Long =
                    dsClient.publishGroup(record.groupId, bytes).id

                override suspend fun publishMailbox(pseudonym: ByteArray, bytes: ByteArray): Long =
                    dsClient.publishMailbox(pseudonym, bytes).id
            },
            outbox = object : SweepOutboxPort {
                override fun newEntryId(): String = newOutboxEntryId()

                override suspend fun record(entry: SweepDurableEntry) {
                    // ADR-0040 obligation 4 — the durable entry AND the MLS
                    // snapshot, in that order, BOTH fail-closed, before any
                    // publish. Unlike the four direct paths there is no
                    // eager-merged N+1 to reset on a failure: the sweep is
                    // STAGED, so the caller's `abandonStagedSweep` is the
                    // rollback and it is per-group.
                    prepareCrashSafeCommit(
                        persistEntryOrThrow = {
                            CommitOutboxStore.putEntry(
                                applicationContext,
                                PendingCommitEntry(
                                    id = entry.id,
                                    accountId = accountIdB64,
                                    groupId = groupIdB64,
                                    // The group deliberately sits at N holding an
                                    // UN-merged commit, so this records the epoch
                                    // the sweep was BUILT ON. `reconcileVerdict`
                                    // never reads it — a sweep entry is routed to
                                    // `resolveOwedVerdict`, whose authority is the
                                    // core's `hasStagedSweep()`.
                                    targetEpoch = record.group.epoch().toString(),
                                    commitB64 = entry.commitB64,
                                    secondArtifactB64 = entry.welcomeEnvelopeB64,
                                    secondArtifactKind = entry.welcomeEnvelopeB64
                                        ?.let { SecondArtifactKind.WELCOME },
                                    isSweep = true,
                                    sweepWelcomeMailboxesB64 = entry.welcomeMailboxesB64,
                                    sweepWelcomeRendezvousB64 = entry.welcomeRendezvousB64,
                                ),
                            )
                        },
                        persistSnapshotOrThrow = { persistSnapshotDurableOrThrow() },
                    )
                }

                // A welcomes-owed entry has already reached a verdict, so it is
                // not this finder's business — re-adjudicating it would
                // re-publish a commit whose order was already read.
                override suspend fun findOwed(): SweepDurableEntry? =
                    findSweepEntry { it.isSweep && !it.acked && !it.sweepWelcomesOwed }

                override suspend fun markWelcomesOwed(id: String, commitMessageId: Long) {
                    // Outbox bookkeeping only: the MLS snapshot was persisted one
                    // line earlier by the sweep's own `persist`, so this
                    // deliberately does NOT go through `prepareCrashSafeCommit`.
                    //
                    // The diagnostic lives HERE rather than in the sweep's catch,
                    // because `CommitterSweep.kt` is host-testable on the plain
                    // JVM where `android.util.Log` is not mocked. It re-throws so
                    // the sweep still sees the failure and takes its non-fatal
                    // branch.
                    try {
                        CommitOutboxStore.markWelcomesOwed(
                            applicationContext,
                            id,
                            commitMessageId,
                        )
                    } catch (cause: Throwable) {
                        android.util.Log.w(
                            "ChatRepository",
                            "ADR-0045: could not record the owed Welcome fan-out for $id" +
                                " — a publish failure below is not retryable",
                            cause,
                        )
                        throw cause
                    }
                }

                override suspend fun findOwedWelcomes(): SweepDurableEntry? =
                    findSweepEntry { it.isSweep && !it.acked && it.sweepWelcomesOwed }

                private fun findSweepEntry(
                    match: (PendingCommitEntry) -> Boolean,
                ): SweepDurableEntry? =
                    try {
                        CommitOutboxStore
                            .loadForGroup(applicationContext, accountIdB64, groupIdB64)
                            .firstOrNull(match)
                            ?.let {
                                SweepDurableEntry(
                                    id = it.id,
                                    commitB64 = it.commitB64,
                                    welcomeEnvelopeB64 = it.secondArtifactB64,
                                    welcomeMailboxesB64 = it.sweepWelcomeMailboxesB64,
                                    welcomeRendezvousB64 = it.sweepWelcomeRendezvousB64,
                                    commitMessageId = it.commitMessageId,
                                )
                            }
                    } catch (_: Throwable) {
                        null
                    }

                override suspend fun ack(
                    id: String,
                    commitMessageId: Long?,
                    welcomeMessageId: Long?,
                ) {
                    ackCommitBestEffort(id, commitMessageId, welcomeMessageId)
                }

                override suspend fun discard(id: String) {
                    try {
                        CommitOutboxStore.deleteEntry(applicationContext, id)
                    } catch (_: Throwable) {
                        // best-effort — the next sweep retries.
                    }
                }
            },
            cursor = object : SweepCursorRef {
                override fun read(): Long = record.cursor

                override fun advance(to: Long) {
                    record.advanceCursor(to)
                }
            },
            actingCommitter = ActingCommitterOracle { ownRank ->
                actingCommitterNow(record.group, groupIdHex, ownRank)
            },
            welcomeMailbox = { signatureKey ->
                uniffi.lakko_ffi_uniffi.IdentityPseudonym
                    .fromIdentity(signatureKey)
                    .use { it.asBytes() }
            },
            wrapWelcome = { welcome -> wrapEnvelope(KIND_WELCOME, welcome) },
            // ADR-0045 S7 — the group-journal half of the fan-out. An external
            // joiner approved through a `/g/` link polls the journal for this and
            // nothing else; the sweep cannot tell which adds those are, so it
            // wraps one per added key.
            wrapRendezvous = { key, welcome ->
                uniffi.lakko_ffi_uniffi.wrapWelcomeRendezvous(key, welcome)
            },
            markOwnPublished = { id -> chatClient.markOwnPublishedCommit(id) },
            // `persistSnapshotBestEffort` calls `Identity.snapshotStorage()`,
            // which serialises the WHOLE provider — every group at once — so it
            // must hold the provider lock or it can capture a SIBLING group
            // mid-operation and persist it torn. Every other caller already runs
            // inside a provider region; the sweep calls this from OUTSIDE one (a
            // merge/abandon region has just closed), so it takes the lock here.
            // It is never reached from inside `exclusiveProvider`, which would
            // self-deadlock a non-reentrant `Mutex`.
            persist = { providerLock.withLock { persistSnapshotBestEffort() } },
            // The verdict apply advances the SAME cursor the poll drives, so it
            // takes the FAIL-CLOSED persist — see [DurableCursorGate]. Called
            // from INSIDE an `exclusiveProvider` region, so it must NOT take
            // `providerLock` (a non-reentrant `Mutex` would self-deadlock); its
            // non-suspending type is what keeps that from being a comment.
            persistApplied = {
                try {
                    persistSnapshotDurableOrThrow()
                } catch (t: Throwable) {
                    android.util.Log.w(
                        "ChatRepository",
                        "ADR-0045 verdict apply: MLS snapshot persist failed — holding " +
                            "the journal cursor so the rows are re-fetched rather than lost",
                        t,
                    )
                    throw t
                }
            },
            encodeB64 = ::outboxB64,
            decodeB64 = ::outboxUnb64,
        )
    }

    public suspend fun sweepPendingCommits(groupIdHex: String) {
        val accountIdB64 = currentAccountIdB64.get() ?: return
        val record = groups[groupIdHex] ?: return
        val groupIdB64 = outboxB64(record.groupId)
        // ADR-0045 obligation 1 — a staged sweep is DURABLE and freezes this
        // group's own membership operations across restarts, so a verdict owed
        // from a previous run must be finished before anything else. It is wired
        // into this EXISTING restart seam rather than into a second one, because
        // two restart paths over the same durable state is how they drift.
        //
        // It runs FIRST for two reasons: the legacy loop below would otherwise
        // re-publish the sweep's own commit as if it were a direct-path entry,
        // and — worse — re-publish its Welcome, which ADR-0045 obligation 2
        // forbids before the verdict. And `reconcileVerdict` would DISCARD it:
        // a staged sweep deliberately leaves the group BELOW the entry's
        // recorded epoch, which for a direct-path entry means a torn write.
        val identity = canonicalIdentity.get()
        if (identity != null) {
            try {
                groupLock(groupIdHex).withLock {
                    resolveOwedVerdict(sweepPortsFor(record, identity, groupIdHex))
                }
            } catch (cause: Throwable) {
                // An unreadable verdict leaves the sweep staged for the next
                // attempt; the poll tail retries continuously.
                android.util.Log.w("ChatRepository", "ADR-0045 owed verdict unresolved", cause)
            }
            // A sweep whose verdict SETTLED but whose Welcome never reached the
            // wire owes only the fan-out. It is invisible to every other recovery
            // path — the merge cleared `hasStagedSweep()` and emptied the pending
            // set, and the legacy loop below filters sweep entries out — so
            // without this the added member stays in every peer's roster,
            // addressed by every later message, permanently unable to construct
            // group state.
            //
            // It runs AFTER the owed verdict on purpose: a still-staged sweep
            // owes a VERDICT, and only once that has merged is a Welcome allowed
            // on the wire at all (ADR-0045 obligation 2). `resolveOwedWelcomes`
            // refuses while anything is staged, so the order here is belt to that
            // braces.
            try {
                groupLock(groupIdHex).withLock {
                    resolveOwedWelcomes(sweepPortsFor(record, identity, groupIdHex))
                }
            } catch (cause: Throwable) {
                // The DS is still unreachable. The durable marker survives, so
                // the next sweep — group open or poll tail — retries the fan-out.
                android.util.Log.w("ChatRepository", "ADR-0045 owed Welcome unresolved", cause)
            }
        }
        val entries = try {
            CommitOutboxStore
                .loadForGroup(applicationContext, accountIdB64, groupIdB64)
                // ADR-0045 — a sweep entry belongs to `resolveOwedVerdict`
                // above, never to the direct-path reconciler.
                .filterNot { it.isSweep }
        } catch (_: Throwable) {
            return
        }
        if (entries.isEmpty()) return
        // G-37 — the per-group lock spans the whole sweep, so a re-publish
        // cannot interleave with a fresh commit or a poll apply on the SAME
        // group (the echo re-arm must land before the next poll sees the
        // re-published commit). The epoch read is the only provider touch here;
        // the re-publishes themselves are network + outbox-store work, so they
        // stay outside the provider lock.
        groupLock(groupIdHex).withLock {
            val currentEpoch = try {
                providerLock.withLock { record.group.epoch() }
            } catch (_: Throwable) {
                return@withLock
            }
            sweepPendingCommitEntries(
                entries = entries,
                currentEpoch = currentEpoch,
                republishCommit = { commit -> dsClient.publishGroup(record.groupId, commit).id },
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
        // G-37 — `groups` is a `synchronizedMap`, whose iterators are NOT
        // self-synchronising: iterating it while another coroutine registers a
        // group can throw ConcurrentModificationException. The old
        // repository-wide mutex made this accidentally safe (every writer held
        // it); per-group locking removes that accident, so the map's own
        // monitor is taken explicitly — matching what `pollAllGroups` and
        // `publishThreadList` already do.
        synchronized(groups) {
            for ((hex, record) in groups) {
                slots[hex] = PersistedGroup(
                    cursor = record.cursor,
                    messages = record.snapshot().map {
                        PersistedMessage.Companion.from(it)
                    },
                )
            }
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
        // ADR-0045 UX §6.2 — the record snapshot is taken under the map's
        // monitor and PROJECTED outside it. `toThread` now touches the `Group`
        // handle (for `isActive`), and while that read is short and takes only
        // the handle's own `Mutex`, blocking on it while holding the `groups`
        // monitor would stall every unrelated lookup behind whatever MLS call
        // happens to hold the handle.
        val records = synchronized(groups) { groups.values.toList() }
        val threads = records.map { it.toThread() }
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
    /** ADR-0024 — the members currently shown as "is typing", by
     *  [MemberIdentity], already filtered against the expiry window. The UI
     *  resolves each identity to a member label (1×1 → bare "typing…"). Empty
     *  when nobody is typing.
     *
     *  Keyed on the signature key, never the MLS leaf: OpenMLS hands a vacated
     *  leaf to the next member added, so a leaf key lets a newcomer inherit —
     *  and clear — a departed member's typing state. */
    val typingMembers: Set<MemberIdentity> = emptySet(),
    /** ADR-0024 — the monotone per-member read watermark. The author renders
     *  "Read" on its OWN message once every OTHER member's watermark `>=` that
     *  message's id.
     *
     *  Keyed on the signature key for the sharper version of the same reason:
     *  the watermark is monotone and is never reset on a membership change, so
     *  a leaf key handed a departed member's entire read history to whoever was
     *  added into their slot, and the "Read" tick then reported messages as
     *  read by somebody who never saw them. */
    val readUpToByMember: Map<MemberIdentity, Long> = emptyMap(),
    /**
     * ADR-0045 UX §6.2 — `true` once this device's own membership has ENDED
     * (`Group.isActive()` is false). Terminal and survives a restart.
     *
     * Read off the live handle in [GroupRecord.toThread] rather than tracked as
     * an event, because nothing in the receive path announces the transition:
     * `processIncomingVerifyingRoles` returns the same content-less `Commit`
     * for a commit that removes our own leaf as for any other. A torn read
     * answers `false` — an unreadable handle is not evidence of an eviction
     * (see [membershipState]).
     */
    val membershipEnded: Boolean = false,
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
    /** ADR-0023 — the original author's IDENTITY: their Ed25519 signature
     *  public key, base64, resolved from the MLS sender leaf AT RECEIPT (see
     *  `AuthorIdentity.kt`). Set for every chat message; our own outgoing rows
     *  carry our own key, stamped at send. The author-only edit/delete gate
     *  compares an op's sender identity against this. `null` ⇒ unknown author
     *  (a row stored before this field existed, an unresolvable sender, or a
     *  system row) — no op can pass the gate against it.
     *
     *  This replaced the author LEAF, which is what let a member added into a
     *  departed member's vacated leaf edit and delete that member's history —
     *  and on Android survived restarts, because the leaf was persisted. */
    val authorKey: MemberIdentity? = null,
    /** ADR-0023 — `true` once a peer-accepted EDIT op replaced
     *  [text]; the UI appends a subtle "(edited)" marker. */
    val edited: Boolean = false,
    /** ADR-0023 — `true` once a peer-accepted DELETE op
     *  tombstoned the message; the UI renders a muted "message
     *  deleted" placeholder and drops the text + attachments. */
    val deleted: Boolean = false,
    /** ADR-0023 — accumulated reactions, attributed per reactor identity.
     *  Rendered as emoji chips with counts; re-sending the same
     *  `(reactor, emoji)` toggles it off. */
    val reactions: List<Reaction> = emptyList(),
)

/** ADR-0023 — one reaction on a [ChatMessage], attributed to its sender's
 *  signature key for dedup + per-user toggle.
 *
 *  Keyed on the identity, never the MLS leaf: a leaf-keyed pair let a newcomer
 *  at a departed member's vacated leaf toggle that member's reaction OFF, and
 *  because removing the last reactor of an emoji drops the pair entirely, the
 *  chip did not merely decrement — it DISAPPEARED. */
public data class Reaction(
    val reactor: MemberIdentity,
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

    /** Guards the two presence registries below. They are immutable maps
     *  swapped under this monitor rather than `Collections.synchronizedMap`s
     *  mutated in place, because the folds that decide them are pure
     *  replace-the-map functions in `AuthorIdentity.kt` — which is what makes
     *  them reachable from a test at all. */
    private val presenceLock = Any()

    /** ADR-0024 — ephemeral per-MEMBER typing state: the local monotonic-clock
     *  time (`SystemClock.elapsedRealtime` millis) at which we last observed a
     *  FRESH `typing:true` from that member. An entry auto-expires
     *  [TYPING_EXPIRY_MS] after its stamp; an explicit `typing:false` removes
     *  it immediately. NEVER persisted (rebuilt purely from live signals).
     *
     *  Keyed on the sender's signature key, never the MLS leaf — see
     *  [foldTyping]. */
    private var typingByMember: Map<MemberIdentity, Long> = emptyMap()

    /** ADR-0024 — monotone per-MEMBER read watermark: the highest read-up-to
     *  messageId each member has asserted. A lower value never overwrites a
     *  higher one (re-sync replay is idempotent). Session state — not
     *  persisted.
     *
     *  Keyed on the sender's signature key, never the MLS leaf — see
     *  [foldReadWatermark]. */
    private var readUpToByMember: Map<MemberIdentity, Long> = emptyMap()

    @Volatile
    var cursor: Long = 0
        private set

    /**
     * ADR-0045 — advance the journal cursor from outside the poll fold.
     *
     * The committer sweep's verdict APPLIES the rows below its own commit, so it
     * must move the cursor over exactly those rows or the poll would re-feed
     * consumed rows to OpenMLS. MONOTONE by construction: a lower value is
     * ignored, so a stale caller can never wind the cursor backward and resurrect
     * rows this record has already folded.
     *
     * The caller holds the per-group lock (and, for the sweep, the provider lock
     * too), which is what makes the read-modify-write atomic against the poll.
     */
    fun advanceCursor(to: Long) {
        if (to > cursor) cursor = to
    }

    /**
     * G-37 — per-group poll latch. Now that the DS fetch runs OUTSIDE
     * the provider lock, two concurrent `pollGroup` calls for the same group could
     * otherwise both fetch from the same cursor and then apply overlapping
     * batches. The loser of this CAS skips entirely (it does not even fetch);
     * the winner clears it in a `finally`, so a failed / cancelled fetch never
     * wedges the group. See [runSplitPoll].
     */
    val pollInFlight: java.util.concurrent.atomic.AtomicBoolean =
        java.util.concurrent.atomic.AtomicBoolean(false)

    fun appendOutgoing(
        text: String,
        attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
        messageId: Long,
        createdAt: String,
        /** ADR-0023 — our own IDENTITY at send time, so a peer's later
         *  edit/delete op of this row passes the gate only if it really comes
         *  from us. Stamped at send from the roster we are a member of right
         *  now, rather than read back from a leaf later: any leaf, ours
         *  included, can be reassigned to a member added afterwards. */
        authorKey: MemberIdentity?,
    ) {
        messages.add(
            ChatMessage(
                messageId = messageId,
                direction = Direction.Outgoing,
                text = text,
                attachments = attachments,
                createdAt = createdAt,
                authorKey = authorKey,
            ),
        )
    }

    /**
     * ADR-0023 — locally apply an op WE just sent (the DS never
     * echoes our own application messages back). The
     * author-only gate is satisfied for our own edit / delete because we
     * authored the target and [ownKey] is the identity that row was stamped
     * with; a reaction toggles our own `(ownKey, emoji)`.
     *
     * An unresolvable [ownKey] falls back to [OWN_REACTION_SENTINEL] for the
     * REACTION path only: the envelope has already been published, so dropping
     * the local apply would leave every peer showing a reaction this device can
     * neither see nor undo. Edit / delete deliberately get no sentinel — it
     * would never equal the target's stored author key, so they fail closed
     * exactly as an unresolvable sender always did.
     */
    fun applyOwnOp(
        op: MessageOp,
        target: Long,
        text: String,
        emoji: String?,
        ownKey: MemberIdentity?,
    ) {
        applyOpInternal(
            op = op,
            target = target,
            text = text,
            emoji = emoji,
            sender = if (op == MessageOp.Reaction) ownKey ?: OWN_REACTION_SENTINEL else ownKey,
        )
    }

    /**
     * Consume a batch of receive-path events — the ONE place a decoded event
     * becomes record state.
     *
     * Two callers, and that is the point. The poll is the obvious one
     * ([pollGroup] calls this directly from its locked apply region). The other
     * is the ADR-0045 committer sweep: its verdict
     * applies every journal row the DS ordered below its own commit and then
     * advances the SAME cursor over them, so those rows are gone from the poll's
     * future forever. A chat message another member sent in the window between the
     * sweep's stand-down re-read and its publish lands in exactly that set, and
     * before this it was decrypted and then thrown away. Feeding both paths
     * through this function is what keeps the two from drifting — every envelope
     * kind is folded here once.
     *
     * Deliberately does NOT touch [cursor], and neither does any other
     * event-folding entry point: since the receive-durability fix, the ONLY way
     * this record's cursor moves on either path is [applyDurableCursorBatch],
     * which refuses to advance it past a batch whose snapshot did not reach disk.
     * A second writer here would not merely let one path wind the other's cursor
     * — it would route around that gate, which is the defect it exists to
     * prevent. See [DurableCursorGate].
     *
     * Events must arrive in journal order; the op fold assumes it.
     */
    fun consumeEvents(
        events: List<IncomingChatEvent>,
        callSignalingSink: ((ByteArray) -> Unit)? = null,
        pushRoutingSecretSink: ((ByteArray, ULong, UInt?) -> Unit)? = null,
        committerBeaconSink: ((UInt?, String) -> Unit)? = null,
        membershipProposalSink: ((ProposalRecord, String) -> Unit)? = null,
    ) {
        for (event in events) {
            when (event) {
                is IncomingChatEvent.Application -> messages.add(
                    ChatMessage(
                        messageId = event.messageId,
                        direction = Direction.Incoming,
                        text = event.text,
                        attachments = event.attachments,
                        createdAt = event.createdAt,
                        // The author IDENTITY the classifier resolved at
                        // receipt, in the epoch that named the leaf. The leaf
                        // itself is deliberately not recorded: by the time this
                        // row is read back it may belong to whoever was added
                        // next.
                        authorKey = event.senderKey,
                    ),
                )
                is IncomingChatEvent.Op -> applyOpInternal(
                    op = event.op,
                    target = event.target,
                    text = event.text,
                    emoji = event.emoji,
                    sender = event.senderKey,
                )
                is IncomingChatEvent.Typing -> applyTyping(
                    sender = event.senderKey,
                    isTyping = event.isTyping,
                    createdAt = event.createdAt,
                )
                is IncomingChatEvent.ReadReceipt -> applyReadReceipt(
                    sender = event.senderKey,
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
                    // leaf the gate needs, and — ADR-0044 — the MINTER's
                    // `mint_epoch` the gate orders it by.
                    pushRoutingSecretSink?.invoke(event.secret, event.mintEpoch, event.senderLeaf)
                }
                is IncomingChatEvent.MembershipProposal -> {
                    // ADR-0045 UX §10 — a membership REQUEST another member
                    // published. Not a timeline row: a request is an intent the
                    // system may discard a second later, and the members sheet
                    // renders it role-gated from `Group.pendingProposals()`,
                    // which stays the source of truth for WHAT is pending.
                    //
                    // What the sink adds is WHEN it landed, which the core
                    // cannot know: the DS row's server-assigned timestamp, the
                    // same value every member reads for the same row. Without
                    // it `requestState` sees no observation and reads WAITING
                    // immediately, so §4's quiet window never applies to a
                    // peer's request — which is every request an admin sees.
                    membershipProposalSink?.invoke(event.record, event.createdAt)
                }
                is IncomingChatEvent.CommitterBeacon -> {
                    // ADR-0045 decision 2 — a committer-liveness beacon. Not a
                    // timeline row: it is an observation about WHICH owner /
                    // admin can sweep the pending proposal set, carried by the
                    // event's `senderLeaf` + `createdAt` — the two things the
                    // field-less envelope deliberately omits, and the only two
                    // the takeover fold consumes. Handed to the sink, which
                    // resolves the leaf to a signature KEY while the roster is
                    // still authenticated in this epoch and files it against the
                    // DS row's server-assigned timestamp. The RANK is derived
                    // nowhere near here: it is a position in the role table's
                    // canonical order and is taken at VERDICT time.
                    committerBeaconSink?.invoke(event.senderLeaf, event.createdAt)
                }
            }
        }
    }

    /**
     * ADR-0023 — fold an edit / delete / reaction op into the
     * target message identified by its DS [target] id.
     *
     *   * edit / delete — **author-only gate** ([editDeleteAuthorized]):
     *     applied only if [sender] equals the target's stored
     *     [ChatMessage.authorKey] (both non-null). Otherwise DROPPED
     *     (fail-closed, invariant #4). An unknown target is also dropped.
     *   * reaction — accepted from any member, attributed to [sender];
     *     TOGGLE via [toggleReaction]: an existing `(reactor, emoji)` pair is
     *     removed, else added. A `null` sender is dropped (no attribution
     *     possible).
     *
     * [sender] is an IDENTITY — a signature key resolved at receipt — and never
     * an MLS leaf index. Comparing leaves here is what let a member added into
     * a departed member's vacated leaf edit and delete that member's history
     * and destroy their reactions; see `AuthorIdentity.kt`. The two rules
     * themselves live there, not here, so a test can reach them.
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
        sender: MemberIdentity?,
    ) {
        synchronized(messages) {
            val idx = messages.indexOfFirst { it.messageId == target }
            if (idx < 0) return // unknown target — drop
            val current = messages[idx]
            when (op) {
                MessageOp.Edit -> {
                    if (!editDeleteAuthorized(current.authorKey, sender)) return
                    if (current.deleted) return // a deleted message stays deleted
                    messages[idx] = current.copy(text = text, edited = true)
                }
                MessageOp.Delete -> {
                    if (!editDeleteAuthorized(current.authorKey, sender)) return
                    messages[idx] = current.copy(
                        text = "",
                        attachments = emptyList(),
                        deleted = true,
                    )
                }
                MessageOp.Reaction -> {
                    val e = emoji ?: return
                    if (sender == null) return
                    messages[idx] = current.copy(
                        reactions = toggleReaction(current.reactions, sender, e),
                    )
                }
            }
        }
    }

    /**
     * ADR-0024 — fold an incoming TYPING signal into the ephemeral per-MEMBER
     * state. The rule is [foldTyping]; this supplies the two host values it
     * deliberately does not read for itself — the DS row's age and the local
     * monotonic clock.
     *
     * A `null` [sender] is dropped here rather than inside the fold: an
     * unattributable signal has nothing to key on.
     */
    fun applyTyping(sender: MemberIdentity?, isTyping: Boolean, createdAt: String) {
        val who = sender ?: return
        synchronized(presenceLock) {
            typingByMember = foldTyping(
                prev = typingByMember,
                sender = who,
                isTyping = isTyping,
                ageMs = typingSignalAgeMs(createdAt),
                nowMs = android.os.SystemClock.elapsedRealtime(),
            )
        }
    }

    /**
     * ADR-0024 — fold an incoming READ-RECEIPT into the monotone per-MEMBER
     * watermark ([foldReadWatermark]). A `null` sender is dropped (no
     * attribution).
     */
    fun applyReadReceipt(sender: MemberIdentity?, upTo: Long) {
        val who = sender ?: return
        synchronized(presenceLock) {
            readUpToByMember = foldReadWatermark(readUpToByMember, who, upTo)
        }
    }

    /**
     * ADR-0024 — the members currently shown as "typing", re-filtered against
     * the expiry window at read time ([pruneTyping]). An entry whose last fresh
     * `typing:true` is older than [TYPING_EXPIRY_MS] is auto-expired (and
     * pruned from the backing map as a side effect).
     */
    fun activeTypingMembers(): Set<MemberIdentity> {
        val now = android.os.SystemClock.elapsedRealtime()
        synchronized(presenceLock) {
            typingByMember = pruneTyping(typingByMember, now)
            return typingByMember.keys.toSet()
        }
    }

    /** ADR-0024 — read-only snapshot of the monotone per-member read
     *  watermark for the author-side read-state render. */
    fun readUpToSnapshot(): Map<MemberIdentity, Long> =
        synchronized(presenceLock) { readUpToByMember }

    fun toThread(): ChatThread = ChatThread(
        groupIdHex = DsClient.pseudonymHex(groupId),
        messages = messages.toList(),
        cursor = cursor,
        typingMembers = activeTypingMembers(),
        readUpToByMember = readUpToSnapshot(),
        // ADR-0045 UX §6.2 — safe to read here, unlike `members()`: OpenMLS
        // `is_active()` is a field read on the in-memory group state and
        // touches no provider storage, and the FFI `Group` guards it with its
        // own `Mutex`. So this is the one group-handle read [toThread] can make
        // outside the repository's `providerLock`.
        membershipEnded = readIsActive { group.isActive() } == false,
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
        SystemKind.ExternalJoinProposal -> "[mls external join]"
    }

    companion object {
        // TYPING_EXPIRY_MS moved to `AuthorIdentity.kt`, beside the folds it
        // bounds: this class is private, so a fold that read it from here
        // could not be reached from a test.

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
